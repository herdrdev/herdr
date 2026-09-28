//! The settings screen: a fixed section list on the left, the selected
//! section's content on the right.
//!
//! The screen grew out of a tab strip, so section switching is still
//! `tab`/`left`/`right` and the list keeps the same open, apply and close
//! affordances. Only the section index moved from a horizontal strip to a
//! vertical column, which is what made room for sections that are lists rather
//! than single choices.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{List, ListItem, ListState, Paragraph},
    Frame,
};

use super::widgets::{
    action_button_row_rects, centered_popup_rect, modal_stack_areas, panel_contrast_fg,
    render_action_button, render_modal_choice_list, render_panel_shell, ActionButtonSpec,
};
use crate::{
    app::{
        settings_general::GeneralRow,
        state::{AppState, Palette, SettingsSection},
    },
    config::{StatusIndicatorStyle, ToastDelivery},
};

/// Requested width. `centered_popup_rect` clamps it, so an 80-column terminal
/// gets a narrower screen rather than a clipped one.
pub(crate) const SETTINGS_POPUP_WIDTH: u16 = 96;
pub(crate) const SETTINGS_POPUP_BASE_HEIGHT: u16 = 26;
/// Width of the section column, including its one-column gutter.
const SETTINGS_NAV_WIDTH: u16 = 20;

pub(crate) fn settings_popup_height(_app: &AppState) -> u16 {
    SETTINGS_POPUP_BASE_HEIGHT
}

/// The areas the settings body is split into. The nav column carries its own
/// filter line, so the section list starts one row lower than the column.
pub(crate) struct SettingsAreas {
    pub(crate) search: Rect,
    pub(crate) nav: Rect,
    pub(crate) content: Rect,
}

/// Pure layout, shared by the renderer and every hit test.
pub(crate) fn settings_areas(inner: Rect) -> SettingsAreas {
    let stack = modal_stack_areas(inner, 2, 2, 0, 1);
    let nav_width = SETTINGS_NAV_WIDTH.min(stack.content.width.saturating_sub(10));
    let [column, _gap, content] = Layout::horizontal([
        Constraint::Length(nav_width),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas(stack.content);
    let [search, nav] = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).areas(column);
    SettingsAreas {
        search,
        nav,
        content,
    }
}

/// The first visible nav row, so the active section is always on screen even
/// when the filtered section list is taller than the popup.
pub(crate) fn settings_nav_scroll(app: &AppState, nav: Rect) -> usize {
    let rows = nav.height as usize;
    let sections = crate::app::state::settings_sections_matching(&app.settings.search);
    let selected = sections
        .iter()
        .position(|section| *section == app.settings.section)
        .unwrap_or_else(|| app.settings.section.index());
    if rows == 0 || selected < rows {
        0
    } else {
        selected + 1 - rows
    }
}

// ---------------------------------------------------------------------------
// General
// ---------------------------------------------------------------------------

/// The y offset of each General row inside the content column's list area.
/// Rows with an explanatory second line are two lines tall.
pub(crate) fn general_row_offsets() -> Vec<(u16, u16)> {
    let mut offsets = Vec::new();
    let mut y = 0;
    for row in GeneralRow::ALL {
        let height = if row.hint().is_some() { 2 } else { 1 };
        offsets.push((y, height));
        y += height;
    }
    offsets
}

/// First visible line in the General list, keeping the selected row fully in
/// view when its labels and hints exceed the fixed popup height.
pub(crate) fn general_row_scroll(selected: usize, height: u16) -> u16 {
    let Some((start, row_height)) = general_row_offsets().get(selected).copied() else {
        return 0;
    };
    start.saturating_add(row_height).saturating_sub(height)
}

fn render_settings_general(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [title, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new("general").style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        title,
    );

    let width = list.width as usize;
    let mut lines = Vec::new();
    for (index, row) in GeneralRow::ALL.iter().enumerate() {
        let selected = index == app.settings.list.selected;
        let value = format!("[{}]", row.value(app));
        let label = row.label();
        let pad = width
            .saturating_sub(label.chars().count() + value.chars().count() + 2)
            .max(1);
        let label_style = if selected {
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(p.subtext0)
        };
        let value_style = if row.is_editable() {
            Style::default().fg(p.accent)
        } else {
            Style::default().fg(p.overlay1)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!("{} {label}", if selected { "▸" } else { " " }),
                label_style,
            ),
            Span::raw(" ".repeat(pad)),
            Span::styled(value, value_style),
        ]));
        if let Some(hint) = row.hint() {
            lines.push(Line::from(Span::styled(
                format!("   {hint}"),
                Style::default().fg(p.overlay1),
            )));
        }
    }
    let scroll = general_row_scroll(app.settings.list.selected, list.height);
    frame.render_widget(Paragraph::new(lines).scroll((scroll, 0)), list);
}

// ---------------------------------------------------------------------------
// Keybindings
// ---------------------------------------------------------------------------

fn render_settings_keybindings(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [title, list] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "keybindings",
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "   ↵ captures the next chord for the selected row",
                Style::default().fg(p.overlay1),
            ),
        ])),
        title,
    );

    let rows = crate::app::settings_keybindings::settings_keybinding_rows(app);
    let key_width = rows
        .iter()
        .map(|row| row.key.chars().count())
        .max()
        .unwrap_or(8);
    let visible = list.height as usize;
    let scroll = app
        .settings
        .list
        .selected
        .saturating_sub(visible.saturating_sub(1));

    let capture = app.settings.keybind_capture.as_ref();
    let mut lines = Vec::new();
    for (index, row) in rows.iter().enumerate().skip(scroll).take(visible) {
        if row.heading {
            lines.push(Line::from(Span::styled(
                format!(" {}", row.label),
                Style::default().fg(p.accent).add_modifier(Modifier::BOLD),
            )));
            continue;
        }
        let selected = index == app.settings.list.selected;
        let capturing = capture.is_some_and(|capture| capture.row == index);
        let base = if selected {
            Style::default().fg(p.text).bg(p.surface0)
        } else {
            Style::default().fg(p.subtext0)
        };
        let key = if capturing {
            "press a chord".to_string()
        } else {
            row.key.clone()
        };
        let key_style = if capturing {
            base.fg(p.yellow).add_modifier(Modifier::BOLD)
        } else {
            base.fg(p.mauve).add_modifier(Modifier::BOLD)
        };
        lines.push(Line::from(vec![
            Span::styled(
                format!(" {:<width$} ", key, width = key_width.max(13)),
                key_style,
            ),
            Span::styled(row.label.clone(), base),
        ]));
        // The refusal belongs on the row that caused it, not in a toast that
        // outlives the capture.
        if let Some(error) = capture
            .filter(|capture| capture.row == index)
            .and_then(|capture| capture.error.as_deref())
        {
            lines.push(Line::from(Span::styled(
                format!("   {error}"),
                Style::default().fg(p.red),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), list);
}

// ---------------------------------------------------------------------------
// Providers, integrations, source control, about, archive
// ---------------------------------------------------------------------------

fn render_probe_section(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    kind: crate::app::probes::ToolProbeKind,
    title: &str,
    description: &str,
) {
    let p = &app.palette;
    let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                title,
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(description, Style::default().fg(p.overlay1))),
        ]),
        heading,
    );

    if app.tool_probes_pending() {
        frame.render_widget(
            Paragraph::new(Span::styled(" checking…", Style::default().fg(p.overlay1))),
            body,
        );
        return;
    }

    let mut lines = Vec::new();
    for probe in app.tool_probes_for(kind) {
        lines.push(probe_line(probe, p));
        // Version alone does not say how Herdr starts the agent; the flag line
        // does, and it comes from the dispatch argv builder.
        if kind == crate::app::probes::ToolProbeKind::Provider {
            lines.push(Line::from(Span::styled(
                format!(
                    "   {}",
                    crate::app::settings_providers::provider_flags_label(app, probe.label)
                ),
                Style::default().fg(p.overlay0),
            )));
        }
    }
    frame.render_widget(Paragraph::new(lines), body);
}

fn probe_line(probe: &crate::app::probes::ToolProbe, p: &Palette) -> Line<'static> {
    let marker_style = match probe.outcome {
        crate::app::probes::ToolProbeOutcome::Ready => Style::default().fg(p.green),
        crate::app::probes::ToolProbeOutcome::NeedsAttention => Style::default().fg(p.yellow),
        crate::app::probes::ToolProbeOutcome::Missing => Style::default().fg(p.overlay0),
        crate::app::probes::ToolProbeOutcome::TimedOut => Style::default().fg(p.red),
    };
    Line::from(vec![
        Span::styled(format!(" {} ", probe.outcome.marker()), marker_style),
        Span::styled(
            format!("{:<10}", probe.label),
            Style::default().fg(p.subtext0),
        ),
        Span::styled(probe.detail.clone(), Style::default().fg(p.overlay1)),
    ])
}

fn render_service_auth(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [heading, body] = Layout::vertical([Constraint::Length(3), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(vec![
            Line::from(Span::styled(
                "service auth",
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            )),
            Line::from(Span::styled(
                "gh, linearis and missive, checked once per session",
                Style::default().fg(p.overlay1),
            )),
        ]),
        heading,
    );

    let mut lines = if app.tool_probes_pending() {
        vec![Line::from(Span::styled(
            " checking…",
            Style::default().fg(p.overlay1),
        ))]
    } else {
        app.tool_probes_for(crate::app::probes::ToolProbeKind::Integration)
            .into_iter()
            .map(|probe| probe_line(probe, p))
            .collect()
    };
    lines.extend(missive_integration_lines(app));
    frame.render_widget(Paragraph::new(lines), body);
}

fn missive_integration_lines(app: &AppState) -> [Line<'static>; 2] {
    let p = &app.palette;
    let team = app
        .settings_missive_team
        .as_deref()
        .filter(|team| !team.trim().is_empty())
        .unwrap_or("not configured");
    let ready = app.settings_missive_token_present && team != "not configured";
    let marker_style = if ready {
        Style::default().fg(p.green)
    } else {
        Style::default().fg(p.yellow)
    };
    let token = if app.settings_missive_token_present {
        "present"
    } else {
        "missing"
    };
    [
        Line::from(vec![
            Span::styled(format!(" {} ", if ready { "✓" } else { "!" }), marker_style),
            Span::styled("missive   ", Style::default().fg(p.subtext0)),
            Span::styled(
                format!("team {team} · token {token}"),
                Style::default().fg(p.overlay1),
            ),
        ]),
        Line::from(Span::styled(
            format!("   {}", missive_observation_label(app)),
            Style::default().fg(p.overlay0),
        )),
    ]
}

fn missive_observation_label(app: &AppState) -> String {
    let observed_at = app.work_index_snapshot.as_ref().and_then(|snapshot| {
        let unavailable = snapshot.unavailable.as_ref().and_then(|unavailable| {
            unavailable.reason(crate::work_index::WorkIndexSource::Missive)
        });
        unavailable.is_none().then_some(snapshot.observed_at)
    });
    let Some(observed_at) = observed_at else {
        return "no successful observation".to_string();
    };
    let Some(now_unix) = app.status_now_unix else {
        return "observed".to_string();
    };
    let Ok(now_unix) = u64::try_from(now_unix) else {
        return "observed unknown".to_string();
    };
    match (std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(now_unix))
        .duration_since(observed_at)
    {
        Ok(elapsed) => format!(
            "observed {} ago",
            crate::work_projection::compact_elapsed(elapsed)
        ),
        Err(_) => "observed unknown".to_string(),
    }
}

fn render_settings_source_control(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new("source control")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        heading,
    );

    let commit_message_model = if app.commit_message_model.trim().is_empty() {
        "unset (git commit as-is)".to_string()
    } else {
        app.commit_message_model.clone()
    };
    let branch_prefix = if app.branch_prefix.is_empty() {
        "none".to_string()
    } else {
        app.branch_prefix.clone()
    };
    let rows = [
        (
            "commit message model",
            commit_message_model,
            "source_control.commit_message_model",
        ),
        (
            "stage all before commit",
            if app.commit_stage_all { "yes" } else { "no" }.to_string(),
            "source_control.commit_stage_all",
        ),
        (
            "default branch prefix",
            branch_prefix,
            "source_control.branch_prefix",
        ),
        (
            "worktree root",
            app.worktree_directory.display().to_string(),
            "worktrees.directory",
        ),
        (
            "pull request merge method",
            app.pr_merge_method.label().to_string(),
            "source_control.merge_method",
        ),
    ];
    let width = body.width as usize;
    let lines = rows
        .iter()
        .map(|(label, value, key)| {
            let value = format!("[{value}]");
            let pad = width
                .saturating_sub(label.chars().count() + value.chars().count() + 2)
                .max(1);
            vec![
                Line::from(vec![
                    Span::styled(format!(" {label}"), Style::default().fg(p.subtext0)),
                    Span::raw(" ".repeat(pad)),
                    Span::styled(value, Style::default().fg(p.overlay1)),
                ]),
                Line::from(Span::styled(
                    format!("   {key}"),
                    Style::default().fg(p.overlay0),
                )),
            ]
        })
        .collect::<Vec<_>>()
        .concat();
    frame.render_widget(Paragraph::new(lines), body);
}

fn render_settings_about(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new("about").style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        heading,
    );

    let version = match crate::build_info::build_id() {
        Some(build_id) => format!("{} ({build_id})", crate::build_info::BASE_VERSION),
        None => crate::build_info::BASE_VERSION.to_string(),
    };
    let lines = vec![
        Line::from(vec![
            Span::styled(" Version", Style::default().fg(p.subtext0)),
            Span::raw("  "),
            Span::styled(version, Style::default().fg(p.text)),
        ]),
        Line::from(vec![
            Span::styled(" Update track", Style::default().fg(p.subtext0)),
            Span::raw("  "),
            Span::styled("fork-only", Style::default().fg(p.accent)),
        ]),
        Line::from(Span::styled(
            "   this build never updates itself; install fork builds by hand",
            Style::default().fg(p.overlay1),
        )),
    ];
    frame.render_widget(Paragraph::new(lines), body);
}

fn render_settings_archive(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [heading, body] = Layout::vertical([Constraint::Length(2), Constraint::Min(0)]).areas(area);
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::styled(
                "archive",
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ),
            Span::styled(
                "   enter restores · d deletes",
                Style::default().fg(p.overlay1),
            ),
        ])),
        heading,
    );

    let entries = app.archive_entries();
    if entries.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                " no settled threads",
                Style::default().fg(p.overlay1),
            )),
            body,
        );
        return;
    }

    let visible = body.height as usize;
    let scroll = app
        .settings
        .list
        .selected
        .saturating_sub(visible.saturating_sub(1));
    let lines = entries
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(index, entry)| {
            let selected = index == app.settings.list.selected;
            let style = if selected {
                Style::default()
                    .fg(p.text)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.subtext0)
            };
            let suffix = if selected && app.settings.archive_delete_armed {
                "  press d again to delete"
            } else {
                ""
            };
            Line::from(vec![
                Span::styled(
                    format!(
                        "{} {:<14}",
                        if selected { "▸" } else { " " },
                        entry.workspace
                    ),
                    style,
                ),
                Span::styled(entry.title.clone(), style),
                Span::styled(suffix, Style::default().fg(p.red)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), body);
}

// ---------------------------------------------------------------------------
// Shell
// ---------------------------------------------------------------------------

pub(super) fn render_settings_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let Some(popup) = centered_popup_rect(area, SETTINGS_POPUP_WIDTH, settings_popup_height(app))
    else {
        return;
    };

    super::dim_background(frame, area);

    let Some(inner) = render_panel_shell(frame, popup, p.accent, p.panel_bg) else {
        return;
    };
    if inner.height < 6 || inner.width < 20 {
        return;
    }

    let stack = modal_stack_areas(inner, 2, 2, 0, 1);
    let header_rows =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas::<2>(stack.header);
    frame.render_widget(
        Paragraph::new(Line::from(vec![Span::styled(
            " settings",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )])),
        header_rows[0],
    );
    let sep = "─".repeat(inner.width as usize);
    frame.render_widget(
        Paragraph::new(Span::styled(&sep, Style::default().fg(p.surface0))),
        header_rows[1],
    );

    let areas = settings_areas(inner);
    render_settings_search(app, frame, areas.search);
    render_settings_nav(app, frame, areas.nav);

    let content_area = areas.content;
    match app.settings.section {
        SettingsSection::General => render_settings_general(app, frame, content_area),
        SettingsSection::Theme => render_settings_theme(app, frame, content_area),
        SettingsSection::Indicators => render_modal_choice_list(
            frame,
            content_area,
            "agent status indicators",
            "choose color dots or distinct symbols for each state",
            &[
                ("color dots  ● ● ● ○ ·", StatusIndicatorStyle::Dots),
                ("distinct symbols  × ◐ ✓ ○ ·", StatusIndicatorStyle::Symbols),
            ],
            app.status_indicators,
            app.settings.list.selected,
            p,
            1,
        ),
        SettingsSection::Sound => render_settings_toggle(
            frame,
            content_area,
            p,
            "sound alerts",
            "play sounds when agents change state in background",
            app.sound_enabled(),
            app.settings.list.selected,
        ),
        SettingsSection::SidebarPanels => render_settings_sidebar_panels(app, frame, content_area),
        SettingsSection::Toast => render_modal_choice_list(
            frame,
            content_area,
            "notification popups",
            "choose where background popup notifications should appear",
            &[
                ("off", ToastDelivery::Off),
                ("inside herdr", ToastDelivery::Herdr),
                ("via terminal", ToastDelivery::Terminal),
                ("via system", ToastDelivery::System),
            ],
            app.toast_delivery(),
            app.settings.list.selected,
            p,
            2,
        ),
        SettingsSection::PaneLabels => render_settings_toggle(
            frame,
            content_area,
            p,
            "agent border labels",
            "show detected agent names in split pane borders",
            app.agent_border_labels_enabled(),
            app.settings.list.selected,
        ),
        SettingsSection::Keybindings => render_settings_keybindings(app, frame, content_area),
        SettingsSection::Providers => render_probe_section(
            app,
            frame,
            content_area,
            crate::app::probes::ToolProbeKind::Provider,
            "providers",
            "agent CLIs found on PATH, with the flags herdr launches them with",
        ),
        SettingsSection::Integrations => render_settings_integrations(app, frame, content_area),
        SettingsSection::SourceControl => render_settings_source_control(app, frame, content_area),
        SettingsSection::Archive => render_settings_archive(app, frame, content_area),
        SettingsSection::About => render_settings_about(app, frame, content_area),
    }

    if let Some(footer_area) = stack.footer {
        let footer_rows = Layout::vertical([Constraint::Length(1), Constraint::Length(1)])
            .areas::<2>(footer_area);
        let primary_label = settings_primary_button_label(app.settings.section);
        let show_primary = settings_show_primary_action(app);
        let (apply_rect, close_rect) =
            settings_button_rects(inner, app.settings.section, show_primary);
        if let Some(apply_rect) = apply_rect {
            render_action_button(
                frame,
                apply_rect,
                Some("↵"),
                primary_label,
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD),
            );
        }
        render_action_button(
            frame,
            close_rect,
            Some("esc"),
            "close",
            Style::default()
                .fg(p.text)
                .bg(p.surface0)
                .add_modifier(Modifier::BOLD),
        );

        frame.render_widget(
            Paragraph::new(Line::from(vec![
                Span::styled(" ↑↓", Style::default().fg(p.overlay0)),
                Span::styled(" select  ", Style::default().fg(p.overlay1)),
                Span::styled("tab", Style::default().fg(p.overlay0)),
                Span::styled(" section  ", Style::default().fg(p.overlay1)),
                Span::styled("↵", Style::default().fg(p.overlay0)),
                Span::styled(" change", Style::default().fg(p.overlay1)),
            ])),
            footer_rows[0],
        );
    }
}

/// The filter line above the section list.
fn render_settings_search(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let query = &app.settings.search;
    let (text, style) = if app.settings.search_active {
        (
            format!("🔍 {query}▏"),
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )
    } else if query.is_empty() {
        (
            "🔍 / to search".to_string(),
            Style::default().fg(p.overlay1),
        )
    } else {
        (format!("🔍 {query}"), Style::default().fg(p.subtext0))
    };
    frame.render_widget(Paragraph::new(Span::styled(text, style)), area);
}

fn render_settings_nav(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let scroll = settings_nav_scroll(app, area);
    let sections = crate::app::state::settings_sections_matching(&app.settings.search);
    if sections.is_empty() {
        frame.render_widget(
            Paragraph::new(Span::styled(" no match", Style::default().fg(p.overlay1))),
            area,
        );
        return;
    }
    let lines = sections
        .iter()
        .skip(scroll)
        .take(area.height as usize)
        .map(|section| {
            let active = *section == app.settings.section;
            let style = if active {
                Style::default()
                    .fg(panel_contrast_fg(p))
                    .bg(p.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.subtext0)
            };
            let badge = if app.settings_section_has_badge(*section) {
                "●"
            } else {
                " "
            };
            Line::from(vec![
                Span::styled(
                    format!(
                        "{} {} {:<width$}",
                        if active { "▸" } else { " " },
                        section.glyph(),
                        section.label(),
                        width = area.width.saturating_sub(6) as usize
                    ),
                    style,
                ),
                Span::styled(badge, Style::default().fg(p.accent)),
            ])
        })
        .collect::<Vec<_>>();
    frame.render_widget(Paragraph::new(lines), area);
}

pub(crate) fn settings_primary_button_label(section: SettingsSection) -> &'static str {
    match section {
        SettingsSection::Integrations => "install",
        _ => "apply",
    }
}

pub(crate) fn settings_show_primary_action(app: &AppState) -> bool {
    match app.settings.section {
        SettingsSection::Integrations => app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install),
        _ => true,
    }
}

pub(crate) fn settings_button_rects(
    inner: Rect,
    section: SettingsSection,
    show_primary: bool,
) -> (Option<Rect>, Rect) {
    if !show_primary {
        let rects = action_button_row_rects(
            inner,
            &[ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            }],
            2,
            inner.height.saturating_sub(1),
        );
        return (None, rects[0]);
    }

    let rects = action_button_row_rects(
        inner,
        &[
            ActionButtonSpec {
                hint: Some("↵"),
                label: settings_primary_button_label(section),
            },
            ActionButtonSpec {
                hint: Some("esc"),
                label: "close",
            },
        ],
        2,
        inner.height.saturating_sub(1),
    );
    (Some(rects[0]), rects[1])
}

fn integrations_footer_paragraph(app: &AppState) -> Paragraph<'static> {
    let p = &app.palette;
    let mut footer_lines = Vec::new();
    if !app.integration_install_messages.is_empty() {
        for message in &app.integration_install_messages {
            footer_lines.push(Line::from(Span::styled(
                format!(" {message}"),
                Style::default().fg(p.overlay1),
            )));
        }
    } else {
        let found_any = app.integration_recommendations.iter().any(|item| {
            item.available || item.state != crate::integration::IntegrationStatusKind::NotInstalled
        });
        let hint = if app
            .integration_recommendations
            .iter()
            .any(crate::integration::IntegrationRecommendation::needs_install)
        {
            " press install to add available or outdated integrations"
        } else if found_any {
            " all detected integrations are installed"
        } else {
            " no supported agent CLIs found on PATH"
        };
        footer_lines.push(Line::from(Span::styled(
            hint.to_string(),
            Style::default().fg(p.overlay1),
        )));
    }
    Paragraph::new(footer_lines).wrap(ratatui::widgets::Wrap { trim: false })
}

fn render_settings_integrations(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;

    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(0),
        Constraint::Length(7),
        Constraint::Length(2),
    ])
    .areas::<5>(area);

    frame.render_widget(
        Paragraph::new("agent integrations")
            .style(Style::default().fg(p.text).add_modifier(Modifier::BOLD)),
        rows[0],
    );
    frame.render_widget(
        Paragraph::new(
            "let agents report state directly instead of relying only on process detection",
        )
        .style(Style::default().fg(p.overlay1))
        .wrap(ratatui::widgets::Wrap { trim: false }),
        rows[1],
    );

    let mut lines = Vec::new();
    for item in &app.integration_recommendations {
        let marker = match item.state {
            crate::integration::IntegrationStatusKind::Current => "✓",
            crate::integration::IntegrationStatusKind::Outdated => "↻",
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => "+",
            crate::integration::IntegrationStatusKind::NotInstalled => "–",
        };
        let marker_style = match item.state {
            crate::integration::IntegrationStatusKind::Current => Style::default().fg(p.green),
            crate::integration::IntegrationStatusKind::Outdated => Style::default().fg(p.yellow),
            crate::integration::IntegrationStatusKind::NotInstalled if item.available => {
                Style::default().fg(p.accent)
            }
            crate::integration::IntegrationStatusKind::NotInstalled => {
                Style::default().fg(p.overlay0)
            }
        };
        lines.push(Line::from(vec![
            Span::styled(format!(" {marker} "), marker_style),
            Span::styled(
                format!("{:<9}", item.label),
                Style::default().fg(p.subtext0),
            ),
            Span::styled(item.status_label(), Style::default().fg(p.overlay1)),
        ]));
    }

    if lines.is_empty() {
        lines.push(Line::from(Span::styled(
            " no integration targets available",
            Style::default().fg(p.overlay1),
        )));
    }

    frame.render_widget(Paragraph::new(lines), rows[2]);
    render_service_auth(app, frame, rows[3]);
    frame.render_widget(integrations_footer_paragraph(app), rows[4]);
}

fn render_settings_theme(app: &AppState, frame: &mut Frame, area: Rect) {
    use crate::app::state::THEME_NAMES;

    let p = &app.palette;
    let items: Vec<ListItem> = THEME_NAMES
        .iter()
        .map(|name| {
            let is_current = name.to_lowercase().replace([' ', '_'], "-")
                == app.theme_name.to_lowercase().replace([' ', '_'], "-");
            let marker = if is_current { " ✓" } else { "" };
            ListItem::new(Line::from(vec![
                Span::styled(*name, Style::default().fg(p.subtext0)),
                Span::styled(marker, Style::default().fg(p.green)),
            ]))
        })
        .collect();

    let list = List::new(items)
        .highlight_style(
            Style::default()
                .bg(p.surface0)
                .fg(p.text)
                .add_modifier(Modifier::BOLD),
        )
        .highlight_symbol(" ▸ ")
        .style(Style::default().fg(p.subtext0));

    let mut state = ListState::default().with_selected(Some(app.settings.list.selected));
    frame.render_stateful_widget(list, area, &mut state);
}

fn render_settings_toggle(
    frame: &mut Frame,
    area: Rect,
    p: &Palette,
    title: &str,
    description: &str,
    current_value: bool,
    selected_idx: usize,
) {
    render_modal_choice_list(
        frame,
        area,
        title,
        description,
        &[("on", true), ("off", false)],
        current_value,
        selected_idx,
        p,
        1,
    );
}

fn render_settings_sidebar_panels(app: &AppState, frame: &mut Frame, area: Rect) {
    let p = &app.palette;
    let [title, description, list] = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Min(0),
    ])
    .areas(area);
    frame.render_widget(
        Paragraph::new(Span::styled(
            "sidebar panels",
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        )),
        title,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "Choose which tabs and widgets appear in the bottom-left dock. Enter or click to toggle.",
            Style::default().fg(p.overlay1),
        )),
        description,
    );

    let items = crate::app::state::settings_sidebar_panel_items(app);
    let visible = list.height as usize;
    let scroll = app
        .settings
        .list
        .selected
        .saturating_sub(visible.saturating_sub(1));
    let rows = items
        .iter()
        .enumerate()
        .skip(scroll)
        .take(visible)
        .map(|(index, item)| {
            let selected = index == app.settings.list.selected;
            let style = if selected {
                Style::default()
                    .fg(p.text)
                    .bg(p.surface0)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(p.subtext0)
            };
            let marker = if item.visible { "[x]" } else { "[ ]" };
            ListItem::new(Line::from(vec![
                Span::styled(format!("{} ", if selected { "▸" } else { " " }), style),
                Span::styled(
                    marker,
                    Style::default().fg(if item.visible { p.accent } else { p.overlay0 }),
                ),
                Span::styled(format!(" {}", item.label), style),
            ]))
        })
        .collect::<Vec<_>>();
    let mut state =
        ListState::default().with_selected(Some(app.settings.list.selected.saturating_sub(scroll)));
    frame.render_stateful_widget(List::new(rows), list, &mut state);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn rendered_integrations_overlay(app: &AppState, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| render_settings_overlay(app, frame, frame.area()))
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .chunks(usize::from(width))
            .map(|row| row.iter().map(|cell| cell.symbol()).collect::<String>())
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_nav_column_and_content_column_never_overlap() {
        for (width, height) in [(76u16, 22u16), (92, 24)] {
            let inner = Rect::new(1, 1, width, height);
            let areas = settings_areas(inner);
            assert!(areas.nav.width > 0);
            assert!(areas.content.width > 0);
            assert!(areas.nav.x + areas.nav.width < areas.content.x);
            assert!(areas.content.x + areas.content.width <= inner.x + inner.width);
        }
    }

    #[test]
    fn the_nav_scrolls_to_keep_the_active_section_visible() {
        let mut app = AppState::test_new();
        let nav = Rect::new(0, 0, 20, 4);
        app.settings.section = SettingsSection::General;
        assert_eq!(settings_nav_scroll(&app, nav), 0);
        app.settings.section = SettingsSection::About;
        assert_eq!(
            settings_nav_scroll(&app, nav),
            SettingsSection::About.index() + 1 - 4
        );
    }

    #[test]
    fn the_nav_keeps_two_columns_and_a_filter_line_at_80_and_120_columns() {
        for (width, height) in [(80u16, 24u16), (120, 40)] {
            let screen = Rect::new(0, 0, width, height);
            let popup =
                centered_popup_rect(screen, SETTINGS_POPUP_WIDTH, SETTINGS_POPUP_BASE_HEIGHT)
                    .expect("settings popup fits");
            let inner = Rect::new(
                popup.x + 1,
                popup.y + 1,
                popup.width.saturating_sub(2),
                popup.height.saturating_sub(2),
            );
            let areas = settings_areas(inner);
            assert_eq!(areas.search.height, 1);
            assert_eq!(areas.search.x, areas.nav.x);
            assert_eq!(areas.search.width, areas.nav.width);
            assert_eq!(areas.nav.y, areas.search.y + 1);
            assert!(areas.nav.width > 0, "nav column at {width}x{height}");
            assert!(
                areas.content.width > 0,
                "content column at {width}x{height}"
            );
            assert!(areas.nav.x + areas.nav.width < areas.content.x);
            assert!(areas.content.x + areas.content.width <= inner.x + inner.width);
        }
    }

    #[test]
    fn the_nav_scroll_follows_the_filtered_section_list() {
        let mut app = AppState::test_new();
        let nav = Rect::new(0, 0, 20, 4);
        app.settings.section = SettingsSection::About;
        assert_eq!(
            settings_nav_scroll(&app, nav),
            SettingsSection::About.index() + 1 - 4
        );
        // Once the filter leaves a short list, the same section is on screen
        // without any scrolling.
        app.settings.search = "about".into();
        assert_eq!(settings_nav_scroll(&app, nav), 0);
    }

    #[test]
    fn general_row_offsets_give_hinted_rows_two_lines() {
        let offsets = general_row_offsets();
        assert_eq!(offsets.len(), GeneralRow::ALL.len());
        assert_eq!(offsets[0], (0, 2));
        assert_eq!(offsets[1], (2, 2));
        assert_eq!(offsets[2], (4, 1));
    }

    #[test]
    fn minimum_height_general_settings_scrolls_default_panel_surfaces_into_view() {
        let mut app = AppState::test_new();
        app.settings.section = SettingsSection::General;
        app.settings.list.selected = GeneralRow::ALL
            .iter()
            .position(|row| *row == GeneralRow::DefaultPanelSurfaces)
            .expect("default panel surfaces row");

        let rendered =
            rendered_integrations_overlay(&app, SETTINGS_POPUP_WIDTH, SETTINGS_POPUP_BASE_HEIGHT);

        assert!(rendered.contains("Default panel surfaces"), "{rendered}");
    }

    #[test]
    fn injected_work_index_snapshot_renders_missive_integration_without_a_pty() {
        let mut app = AppState::test_new();
        app.settings.section = SettingsSection::Integrations;
        app.tool_probes = crate::app::probes::ToolProbeState::Ready(vec![
            crate::app::probes::ToolProbe {
                label: "gh",
                kind: crate::app::probes::ToolProbeKind::Integration,
                outcome: crate::app::probes::ToolProbeOutcome::Ready,
                detail: "authenticated".into(),
            },
            crate::app::probes::ToolProbe {
                label: "linearis",
                kind: crate::app::probes::ToolProbeKind::Integration,
                outcome: crate::app::probes::ToolProbeOutcome::Ready,
                detail: "authenticated".into(),
            },
        ]);
        app.settings_missive_team = Some("support".into());
        app.settings_missive_token_present = true;
        app.status_now_unix = Some(180);
        app.work_index_snapshot = Some(crate::work_index::Snapshot {
            items: Vec::new(),
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::UNIX_EPOCH + std::time::Duration::from_secs(60),
        });

        for (width, height) in [(80, 24), (120, 40)] {
            let text = rendered_integrations_overlay(&app, width, height);
            for expected in [
                "gh",
                "linearis",
                "missive",
                "team support · token present",
                "observed 2m ago",
            ] {
                assert!(
                    text.contains(expected),
                    "missing {expected:?} at {width}x{height}\n{text}"
                );
            }
        }
    }
}
