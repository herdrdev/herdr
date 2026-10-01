use ratatui::{
    layout::Rect,
    style::Style,
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use crate::app::{
    state::{ControlId, SidebarFooterItem, StatusSegmentKind},
    AppState,
};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum TooltipPlacement {
    Below,
    Above,
}

pub(crate) fn tooltip_rect(
    anchor: Rect,
    label: &str,
    area: Rect,
) -> Option<(Rect, TooltipPlacement)> {
    if anchor.width == 0 || anchor.height == 0 || area.width < 3 || area.height < 3 {
        return None;
    }
    let content_width = label
        .lines()
        .map(crate::ui::text::display_width_u16)
        .max()
        .unwrap_or(0);
    let width = content_width.saturating_add(2).min(area.width).max(3);
    let height = u16::try_from(wrap_tooltip_label(label, width.saturating_sub(2)).len())
        .ok()?
        .saturating_add(2);
    if height > area.height {
        return None;
    }
    let x = anchor.x.max(area.x).min(area.right().saturating_sub(width));
    if anchor.bottom().saturating_add(height) <= area.bottom() {
        return Some((
            Rect::new(x, anchor.bottom(), width, height),
            TooltipPlacement::Below,
        ));
    }
    if anchor.y >= area.y.saturating_add(height) {
        return Some((
            Rect::new(x, anchor.y.saturating_sub(height), width, height),
            TooltipPlacement::Above,
        ));
    }
    None
}

fn rect_contains(rect: Rect, col: u16, row: u16) -> bool {
    rect.width > 0
        && rect.height > 0
        && col >= rect.x
        && col < rect.right()
        && row >= rect.y
        && row < rect.bottom()
}

fn wrap_tooltip_label(label: &str, max_width: u16) -> Vec<String> {
    let max_width = usize::from(max_width).max(1);
    let mut wrapped = Vec::new();
    for paragraph in label.lines() {
        let mut current = String::new();
        for word in paragraph.split_whitespace() {
            let word_width = crate::ui::text::display_width(word);
            if current.is_empty() {
                if word_width <= max_width {
                    current.push_str(word);
                    continue;
                }
            } else if crate::ui::text::display_width(&current)
                .saturating_add(1)
                .saturating_add(word_width)
                <= max_width
            {
                current.push(' ');
                current.push_str(word);
                continue;
            } else {
                wrapped.push(std::mem::take(&mut current));
                if word_width <= max_width {
                    current.push_str(word);
                    continue;
                }
            }

            for character in word.chars() {
                let character_width = crate::ui::text::display_width(&character.to_string());
                if !current.is_empty()
                    && crate::ui::text::display_width(&current).saturating_add(character_width)
                        > max_width
                {
                    wrapped.push(std::mem::take(&mut current));
                }
                current.push(character);
            }
        }
        if current.is_empty() && paragraph.is_empty() {
            wrapped.push(String::new());
        } else if !current.is_empty() {
            wrapped.push(current);
        }
    }
    if wrapped.is_empty() {
        wrapped.push(String::new());
    }
    wrapped
}

pub(crate) fn hovered_control_at(app: &AppState, col: u16, row: u16) -> Option<ControlId> {
    let view = &app.view;
    if app.config_diagnostic.is_some() && rect_contains(view.config_diagnostic_hit_area, col, row) {
        return Some(ControlId::ConfigDiagnostic);
    }
    let fixed = [
        (
            ControlId::SidebarAnimationPause,
            view.hyperspace_pause_hit_area,
        ),
        (
            ControlId::NotepadUsageToggle,
            view.notepad_usage_toggle_hit_area,
        ),
        (ControlId::DockClose, view.dock_tab_close_rect),
        (ControlId::DockAdd, view.dock_plus_rect),
        (ControlId::DockAutoOpen, view.dock_auto_open_rect),
        (ControlId::TopBarScrollLeft, view.tab_scroll_left_hit_area),
        (ControlId::TopBarScrollRight, view.tab_scroll_right_hit_area),
        (ControlId::TopBarNewTab, view.new_tab_hit_area),
        (
            ControlId::TopBarRepoEditor,
            view.repo_editor_button_hit_area,
        ),
        (ControlId::TopBarAddAction, view.add_action_button_hit_area),
        (ControlId::TopBarGitMenu, view.git_menu_button_hit_area),
        (ControlId::TopBarPaneBelow, view.pane_toggle_below_hit_area),
        (ControlId::TopBarPaneRight, view.pane_toggle_right_hit_area),
        (
            ControlId::SidebarStarFilter,
            super::sidebar_header_star_filter_rect(view.sidebar_rect),
        ),
        (
            ControlId::SidebarNewThread,
            super::sidebar_header_new_thread_rect(view.sidebar_rect),
        ),
        (
            ControlId::SidebarNewMenu,
            super::sidebar_header_new_menu_rect(view.sidebar_rect),
        ),
        (
            ControlId::SidebarMore,
            super::sidebar_header_overflow_rect(view.sidebar_rect),
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Settings),
            view.sidebar_footer_settings_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::AskSubtitles),
            view.sidebar_footer_ask_subtitles_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::PullRequests),
            view.sidebar_footer_work_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Usage),
            view.sidebar_footer_usage_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Linear),
            view.sidebar_footer_ticket_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Missive),
            view.sidebar_footer_missive_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Refresh),
            view.sidebar_footer_refresh_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Board),
            view.sidebar_footer_board_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::PlanningLock),
            view.sidebar_footer_planning_lock_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::WindowCycleMode),
            view.window_cycle_mode_hit_area,
        ),
        (
            ControlId::SidebarFooter(SidebarFooterItem::Notifications),
            view.notification_hit_area,
        ),
    ];
    fixed
        .into_iter()
        .find_map(|(control, rect)| rect_contains(rect, col, row).then_some(control))
        .or_else(|| {
            view.status_buttons
                .iter()
                .enumerate()
                .find_map(|(index, button)| {
                    rect_contains(button.rect, col, row).then_some(ControlId::StatusButton(index))
                })
        })
        .or_else(|| {
            view.status_segment_hit_areas
                .iter()
                .find_map(|(kind, rect)| {
                    rect_contains(*rect, col, row).then_some(ControlId::StatusSegment(*kind))
                })
        })
        .or_else(|| {
            view.user_action_hit_areas.iter().find_map(|(index, rect)| {
                rect_contains(*rect, col, row).then_some(ControlId::TopBarUserAction(*index))
            })
        })
        .or_else(|| {
            view.dock_tab_hit_areas
                .iter()
                .enumerate()
                .find_map(|(index, rect)| {
                    rect_contains(*rect, col, row).then_some(ControlId::DockTab(index))
                })
        })
        .or_else(|| {
            view.sidebar_hover_targets
                .iter()
                .enumerate()
                .find_map(|(index, target)| {
                    rect_contains(target.rect, col, row).then_some(if target.row_hover {
                        ControlId::SidebarRowHover(target.rect.y)
                    } else {
                        ControlId::SidebarHover(index)
                    })
                })
        })
        .or_else(|| {
            view.notepad_usage_hit_areas
                .iter()
                .enumerate()
                .find_map(|(index, rect)| {
                    rect_contains(*rect, col, row).then_some(ControlId::NotepadUsageRow(index))
                })
        })
}

fn tooltip_target(app: &AppState, control: ControlId) -> Option<(Rect, String)> {
    let view = &app.view;
    let target = match control {
        ControlId::ConfigDiagnostic => return None,
        ControlId::NotepadUsageRow(index) => (
            *view.notepad_usage_hit_areas.get(index)?,
            view.notepad_usage_rows.get(index)?.tooltip.clone()?,
        ),
        ControlId::NotepadUsageToggle => (
            view.notepad_usage_toggle_hit_area,
            if app.notepad.usage_collapsed {
                "Expand usage".into()
            } else {
                "Collapse usage".into()
            },
        ),
        ControlId::SidebarStarFilter => (
            super::sidebar_header_star_filter_rect(view.sidebar_rect),
            if app.sidebar_starred_only {
                "Show all sessions".into()
            } else {
                "Show only starred sessions".into()
            },
        ),
        ControlId::SidebarNewThread => (
            super::sidebar_header_new_thread_rect(view.sidebar_rect),
            "New thread".into(),
        ),
        ControlId::SidebarNewMenu => (
            super::sidebar_header_new_menu_rect(view.sidebar_rect),
            "New…".into(),
        ),
        ControlId::SidebarMore => (
            super::sidebar_header_overflow_rect(view.sidebar_rect),
            "More".into(),
        ),
        ControlId::SidebarFooter(item) => {
            let (rect, label) = match item {
                SidebarFooterItem::Settings => (view.sidebar_footer_settings_hit_area, "Settings"),
                SidebarFooterItem::AskSubtitles => (
                    view.sidebar_footer_ask_subtitles_hit_area,
                    if app.sidebar_show_ask_subtitles {
                        "Hide ask subtitles"
                    } else {
                        "Show ask subtitles"
                    },
                ),
                SidebarFooterItem::PullRequests => {
                    (view.sidebar_footer_work_hit_area, "Pull requests")
                }
                SidebarFooterItem::Usage => (view.sidebar_footer_usage_hit_area, "Usage"),
                SidebarFooterItem::Linear => (view.sidebar_footer_ticket_hit_area, "Linear"),
                SidebarFooterItem::Missive => (view.sidebar_footer_missive_hit_area, "Missive"),
                SidebarFooterItem::Refresh => (view.sidebar_footer_refresh_hit_area, "Refresh"),
                SidebarFooterItem::Board => (view.sidebar_footer_board_hit_area, "Focus board"),
                SidebarFooterItem::PlanningLock => (
                    view.sidebar_footer_planning_lock_hit_area,
                    if app.planning_lock.configured() {
                        "Planning lock settings"
                    } else {
                        "Enable planning lock"
                    },
                ),
                SidebarFooterItem::WindowCycleMode => {
                    (view.window_cycle_mode_hit_area, "Window cycle settings")
                }
                SidebarFooterItem::Notifications => (
                    view.notification_hit_area,
                    if app.notifications_enabled() {
                        "Mute notifications"
                    } else {
                        "Turn on notifications"
                    },
                ),
            };
            (rect, label.into())
        }
        ControlId::SidebarAnimationPause => (
            view.hyperspace_pause_hit_area,
            if app.hyperspace.paused() {
                "Resume the sidebar animation".into()
            } else {
                "Pause the sidebar animation".into()
            },
        ),
        ControlId::DockTab(index) => (
            view.dock_tab_hit_areas.get(index).copied()?,
            app.dock_tab_title(index),
        ),
        ControlId::SidebarHover(index) => {
            let target = view.sidebar_hover_targets.get(index)?;
            (target.rect, target.label.clone())
        }
        ControlId::SidebarRowHover(_) => return None,
        ControlId::DockClose => (view.dock_tab_close_rect, "Close tab".into()),
        ControlId::DockAdd => (view.dock_plus_rect, "Open surface".into()),
        ControlId::DockAutoOpen => (
            view.dock_auto_open_rect,
            if app.open_dock_on_work_link {
                "Opening automatically: on".into()
            } else {
                "Opening automatically: off".into()
            },
        ),
        ControlId::TopBarScrollLeft => (view.tab_scroll_left_hit_area, "Scroll tabs left".into()),
        ControlId::TopBarScrollRight => {
            (view.tab_scroll_right_hit_area, "Scroll tabs right".into())
        }
        ControlId::TopBarNewTab => (view.new_tab_hit_area, "New tab".into()),
        ControlId::TopBarRepoEditor => (view.repo_editor_button_hit_area, "Open in nvim".into()),
        ControlId::TopBarAddAction => (view.add_action_button_hit_area, "Add action".into()),
        ControlId::TopBarUserAction(index) => (
            view.user_action_hit_areas
                .iter()
                .find_map(|(candidate, rect)| (*candidate == index).then_some(*rect))?,
            app.keybinds.user_actions.get(index)?.name.clone(),
        ),
        ControlId::TopBarGitMenu => (view.git_menu_button_hit_area, "Pull".into()),
        ControlId::TopBarPaneBelow => (
            view.pane_toggle_below_hit_area,
            if app
                .pane_toggle_sibling(crate::app::state::PaneToggleDirection::Below)
                .is_some()
            {
                "Close bottom pane"
            } else {
                "Split below"
            }
            .into(),
        ),
        ControlId::TopBarPaneRight => (
            view.pane_toggle_right_hit_area,
            if app
                .pane_toggle_sibling(crate::app::state::PaneToggleDirection::Right)
                .is_some()
            {
                "Close right pane"
            } else {
                "Split right"
            }
            .into(),
        ),
        ControlId::StatusSegment(kind) => (
            view.status_segment_hit_areas
                .iter()
                .find_map(|(candidate, rect)| (*candidate == kind).then_some(*rect))?,
            status_segment_tooltip(app, kind),
        ),
        ControlId::StatusButton(index) => {
            let button = view.status_buttons.get(index)?;
            (
                button.rect,
                match button.action {
                    crate::app::state::StatusButtonAction::Home => {
                        "Home: overview of all workspaces"
                    }
                    crate::app::state::StatusButtonAction::Work => {
                        "Work view: branches and pull requests"
                    }
                    crate::app::state::StatusButtonAction::BlockedFilter => {
                        "Filter to blocked agents"
                    }
                    crate::app::state::StatusButtonAction::Attention => "Agents needing you",
                    crate::app::state::StatusButtonAction::Dock => "Toggle dock panel",
                }
                .into(),
            )
        }
    };
    Some(target)
}

/// Names a status-row segment and where its value comes from.
pub(crate) fn status_segment_tooltip(app: &AppState, kind: StatusSegmentKind) -> String {
    let metrics = app
        .status_metrics
        .as_ref()
        .map(|snapshot| &snapshot.metrics);
    match kind {
        StatusSegmentKind::StatusDetail => if app.status_bar_expanded {
            "Extended status details"
        } else {
            "Simple status details"
        }
        .into(),
        StatusSegmentKind::Link => if app.connectivity.is_online() {
            "Link: internet reachable"
        } else {
            "Link: offline, internet unreachable"
        }
        .into(),
        StatusSegmentKind::Agents => {
            let (agents, blocked) = app.agent_dot_counts();
            format!("Agents on this machine: {agents} active, {blocked} waiting on you")
        }
        StatusSegmentKind::FleetLabel => if app.fleet_status {
            "Fleet hosts"
        } else {
            "Fleet hosts are hidden. Click to show them"
        }
        .into(),
        StatusSegmentKind::FleetDevice(idx) => {
            let Some(machine) = app
                .machines
                .iter()
                .filter(|machine| !machine.is_local())
                .nth(idx)
            else {
                return "Fleet host".into();
            };
            let host = app
                .fleet_snapshot
                .hosts
                .iter()
                .find(|host| host.name == machine.name);
            let status = match host.map(|host| host.state) {
                Some(crate::fleet::HostState::Reachable) => format!(
                    "online · {} agents",
                    host.map_or(0, |host| host.entries.len())
                ),
                Some(crate::fleet::HostState::Unreachable) => "offline".into(),
                Some(crate::fleet::HostState::VersionSkew) => "version skew".into(),
                None => "status unknown".into(),
            };
            let current = current_home_quota_source(app);
            let providers = [
                (crate::app::launch_profiles::QuotaSource::Agy, "Antigravity"),
                (crate::app::launch_profiles::QuotaSource::Codex, "Codex"),
                (crate::app::launch_profiles::QuotaSource::Kimi, "OpenCode"),
            ]
            .into_iter()
            .map(|(provider, label)| {
                if current == Some(provider) {
                    format!("● {label} (current)")
                } else {
                    format!("○ {label}")
                }
            })
            .collect::<Vec<_>>()
            .join(" · ");
            format!("{} · {status}\nProviders: {providers}", machine.name)
        }
        StatusSegmentKind::FleetUseMachine(idx) => {
            let machine = app
                .machines
                .iter()
                .filter(|machine| !machine.is_local())
                .nth(idx);
            match machine {
                Some(machine) if best_home_profile(app).is_some() => format!(
                    "Use {} and its best available model for the next Home launch",
                    machine.name
                ),
                Some(machine) => format!(
                    "{} · no configured provider currently reports remaining usage",
                    machine.name
                ),
                None => "Set the next Home launch machine and model".into(),
            }
        }
        StatusSegmentKind::RemoteHost => match app.view.focused_remote_host.as_deref() {
            Some(host) => format!("Remote device this pane runs on: {host}"),
            None => "Remote device this pane runs on".into(),
        },
        StatusSegmentKind::Hostname => match metrics {
            Some(metrics) => format!("Host herdr runs on: {}", metrics.hostname),
            None => "Host herdr runs on".into(),
        },
        StatusSegmentKind::Cpu => "CPU busy, sampled from kernel ticks".into(),
        StatusSegmentKind::Memory => "Memory used of installed total".into(),
        StatusSegmentKind::Disk => "Disk usage on / (shown above 80%)".into(),
    }
}

fn current_home_quota_source(app: &AppState) -> Option<crate::app::launch_profiles::QuotaSource> {
    let profile_id = app
        .home
        .as_ref()
        .map(|home| home.profile().id.as_str())
        .or(app.next_home_profile.as_deref());
    profile_id
        .and_then(|id| app.launch_profiles.iter().find(|profile| profile.id == id))
        .and_then(|profile| profile.quota)
}

fn best_home_profile(app: &AppState) -> Option<&crate::app::launch_profiles::LaunchProfile> {
    app.launch_profiles.iter().find(|profile| {
        profile.quota.is_some_and(|quota| {
            quota
                .usage(&app.provider_usage)
                .peak_percent()
                .is_some_and(|used| used < 100)
        })
    })
}

pub(super) fn render_hover_tooltip(app: &AppState, frame: &mut Frame) {
    if !app.hover_tooltip_visible {
        return;
    }
    let Some(control) = app.hovered_control else {
        return;
    };
    let Some((anchor, label)) = tooltip_target(app, control) else {
        return;
    };
    let Some((area, _)) = tooltip_rect(anchor, &label, frame.area()) else {
        return;
    };
    let age_tooltip = label.starts_with("Last reply ") || label.starts_with("Last status report ");
    let lines = wrap_tooltip_label(&label, area.width.saturating_sub(2));
    let paragraph = Paragraph::new(ratatui::text::Text::from(
        lines
            .into_iter()
            .enumerate()
            .map(|(index, line)| {
                let color = if age_tooltip && index == 0 {
                    app.palette.yellow
                } else if age_tooltip {
                    app.palette.overlay0
                } else {
                    app.palette.text
                };
                ratatui::text::Line::from(ratatui::text::Span::styled(
                    line,
                    Style::default().fg(color),
                ))
            })
            .collect::<Vec<_>>(),
    ))
    .style(
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.panel_bg),
    )
    .block(Block::default().borders(Borders::ALL));
    frame.render_widget(Clear, area);
    frame.render_widget(paragraph, area);
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn tooltip_prefers_below_and_falls_back_above_without_covering_anchor() {
        let area = Rect::new(0, 0, 40, 10);
        let anchor = Rect::new(35, 1, 2, 1);
        let (below, placement) = tooltip_rect(anchor, "Settings", area).expect("below");
        assert_eq!(placement, TooltipPlacement::Below);
        assert_eq!(below.y, anchor.bottom());
        assert!(below.right() <= area.right());

        let anchor = Rect::new(2, 9, 2, 1);
        let (above, placement) = tooltip_rect(anchor, "Settings", area).expect("above");
        assert_eq!(placement, TooltipPlacement::Above);
        assert_eq!(above.bottom(), anchor.y);
    }

    #[test]
    fn multiline_tooltip_geometry_accounts_for_every_line() {
        let anchor = Rect::new(2, 2, 1, 1);
        let label = "5h window: no data\n7d window: 71% used";
        let (rect, placement) = tooltip_rect(anchor, label, Rect::new(0, 0, 80, 20))
            .expect("room for both window lines");

        assert_eq!(placement, TooltipPlacement::Below);
        assert_eq!(rect.y, anchor.bottom());
        assert_eq!(rect.height, 4);
    }

    #[test]
    fn hover_timer_rests_for_four_hundred_ms_and_dismisses() {
        let mut app = AppState::test_new();
        let started = std::time::Instant::now();
        app.set_hovered_control_at(Some(ControlId::SidebarNewThread), started);
        let original_deadline = app.hover_tooltip_deadline();
        app.set_hovered_control_at(
            Some(ControlId::SidebarNewThread),
            started + std::time::Duration::from_millis(200),
        );
        assert_eq!(app.hover_tooltip_deadline(), original_deadline);
        assert!(!app.reveal_hover_tooltip_at(
            started + crate::app::HOVER_TOOLTIP_DELAY - std::time::Duration::from_millis(1)
        ));
        assert!(app.reveal_hover_tooltip_at(started + crate::app::HOVER_TOOLTIP_DELAY));
        assert!(app.hover_tooltip_visible);

        app.set_hovered_control_at(Some(ControlId::SidebarMore), started);
        assert!(!app.hover_tooltip_visible);
        app.clear_hovered_control();
        assert_eq!(app.hovered_control, None);
        assert_eq!(app.hover_tooltip_deadline(), None);
    }

    #[test]
    fn control_hit_test_covers_header_footer_dock_and_top_bar() {
        let mut app = AppState::test_new();
        app.sidebar_show_ask_subtitles = true;
        app.view.sidebar_rect = Rect::new(0, 0, 26, 24);
        app.view.sidebar_footer_settings_hit_area = Rect::new(1, 23, 2, 1);
        app.view.sidebar_footer_ask_subtitles_hit_area = Rect::new(0, 23, 1, 1);
        app.view.notification_hit_area = Rect::new(14, 23, 2, 1);
        app.view.dock_tab_close_rect = Rect::new(90, 1, 1, 1);
        app.view.add_action_button_hit_area = Rect::new(70, 0, 10, 1);

        assert_eq!(
            hovered_control_at(&app, 1, 23),
            Some(ControlId::SidebarFooter(SidebarFooterItem::Settings))
        );
        assert_eq!(
            hovered_control_at(&app, 0, 23),
            Some(ControlId::SidebarFooter(SidebarFooterItem::AskSubtitles))
        );
        assert_eq!(
            tooltip_target(
                &app,
                ControlId::SidebarFooter(SidebarFooterItem::AskSubtitles)
            )
            .map(|(_, label)| label),
            Some("Hide ask subtitles".to_string())
        );
        app.sidebar_show_ask_subtitles = false;
        assert_eq!(
            tooltip_target(
                &app,
                ControlId::SidebarFooter(SidebarFooterItem::AskSubtitles)
            )
            .map(|(_, label)| label),
            Some("Show ask subtitles".to_string())
        );
        assert_eq!(
            hovered_control_at(&app, 14, 23),
            Some(ControlId::SidebarFooter(SidebarFooterItem::Notifications))
        );
        assert_eq!(hovered_control_at(&app, 90, 1), Some(ControlId::DockClose));
        assert_eq!(
            hovered_control_at(&app, 71, 0),
            Some(ControlId::TopBarAddAction)
        );
        let header = super::super::sidebar_header_new_thread_rect(app.view.sidebar_rect);
        assert_eq!(
            hovered_control_at(&app, header.x, header.y),
            Some(ControlId::SidebarNewThread)
        );
        assert_eq!(
            tooltip_target(&app, ControlId::SidebarNewMenu).map(|(_, label)| label),
            Some("New…".to_string())
        );
    }
    #[test]
    fn a_sidebar_row_glyph_resolves_to_its_own_explanation() {
        let mut app = AppState::test_new();
        app.view.sidebar_rect = Rect::new(0, 0, 26, 24);
        app.view.sidebar_hover_targets = vec![
            crate::app::state::SidebarHoverTarget {
                rect: Rect::new(1, 4, 3, 1),
                label: "Blocked, waiting on you".into(),
                action: None,
                row_hover: false,
            },
            crate::app::state::SidebarHoverTarget {
                rect: Rect::new(4, 5, 1, 1),
                label: "In Review".into(),
                action: None,
                row_hover: false,
            },
            crate::app::state::SidebarHoverTarget {
                rect: Rect::new(1, 8, 20, 1),
                label: String::new(),
                action: None,
                row_hover: true,
            },
        ];

        assert_eq!(
            hovered_control_at(&app, 2, 4),
            Some(ControlId::SidebarHover(0))
        );
        assert_eq!(
            hovered_control_at(&app, 4, 5),
            Some(ControlId::SidebarHover(1))
        );
        assert_eq!(hovered_control_at(&app, 9, 5), None);
        assert_eq!(
            hovered_control_at(&app, 10, 8),
            Some(ControlId::SidebarRowHover(8))
        );
        assert!(tooltip_target(&app, ControlId::SidebarRowHover(8)).is_none());

        let (anchor, label) =
            tooltip_target(&app, ControlId::SidebarHover(1)).expect("status tooltip");
        assert_eq!(anchor, Rect::new(4, 5, 1, 1));
        assert_eq!(label, "In Review");

        // A row that scrolled away between hover and render explains nothing
        // rather than explaining the wrong row.
        app.view.sidebar_hover_targets.clear();
        assert!(tooltip_target(&app, ControlId::SidebarHover(1)).is_none());
    }

    #[test]
    fn usage_row_and_toggle_hit_areas_resolve_to_their_own_tooltips() {
        let mut app = AppState::test_new();
        app.provider_usage
            .accounts
            .push(crate::provider_usage::ProviderAccountUsage {
                provider: crate::provider_usage::QuotaProvider::Claude,
                profile_id: "default".into(),
                label: "primary".into(),
                usage: crate::provider_usage::AccountUsage::default(),
            });
        app.notepad.set_visible_tabs(vec!["usage".to_string()]);
        assert!(app.notepad.usage_tab || app.notepad.select_usage_tab());
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));
        assert_eq!(app.view.notepad_usage_hit_areas.len(), 1);
        app.notepad
            .usage_expanded_providers
            .insert(crate::provider_usage::QuotaProvider::Claude);
        crate::ui::compute_view(&mut app, Rect::new(0, 0, 120, 40));

        let header = app.view.notepad_usage_hit_areas[0];
        assert_eq!(
            hovered_control_at(&app, header.x, header.y),
            Some(ControlId::NotepadUsageRow(0))
        );
        assert_eq!(
            tooltip_target(&app, ControlId::NotepadUsageRow(0))
                .expect("provider header tooltip")
                .1,
            "claude quota: bars show % of each window used; 5h = rolling five-hour window, 7d = weekly window"
        );

        let row = app.view.notepad_usage_hit_areas[1];
        assert_eq!(row.width, app.view.notepad_rect.width);
        assert_eq!(
            hovered_control_at(&app, row.right().saturating_sub(1), row.y),
            Some(ControlId::NotepadUsageRow(1))
        );
        assert!(tooltip_target(&app, ControlId::NotepadUsageRow(1))
            .expect("account row tooltip")
            .1
            .starts_with("5h window: no data\n7d window: no data"));

        let toggle = app.view.notepad_usage_toggle_hit_area;
        assert_eq!(
            hovered_control_at(&app, toggle.x, toggle.y),
            Some(ControlId::NotepadUsageToggle)
        );
        assert_eq!(
            tooltip_target(&app, ControlId::NotepadUsageToggle).map(|(_, label)| label),
            Some("Expand usage".to_string())
        );
    }
}

#[cfg(test)]
mod status_segments {
    use super::*;
    use crate::app::state::StatusSegmentKind;
    use ratatui::layout::Rect;

    const ALL: [StatusSegmentKind; 11] = [
        StatusSegmentKind::StatusDetail,
        StatusSegmentKind::Link,
        StatusSegmentKind::Agents,
        StatusSegmentKind::FleetLabel,
        StatusSegmentKind::FleetDevice(0),
        StatusSegmentKind::FleetUseMachine(0),
        StatusSegmentKind::RemoteHost,
        StatusSegmentKind::Hostname,
        StatusSegmentKind::Cpu,
        StatusSegmentKind::Memory,
        StatusSegmentKind::Disk,
    ];

    #[test]
    fn every_status_segment_names_itself_and_its_source() {
        let mut app = AppState::test_new();
        app.view.status_segment_hit_areas = ALL
            .iter()
            .enumerate()
            .map(|(index, kind)| (*kind, Rect::new(index as u16 * 4, 0, 3, 1)))
            .collect();
        for (index, kind) in ALL.iter().enumerate() {
            let control = ControlId::StatusSegment(*kind);
            assert_eq!(
                hovered_control_at(&app, index as u16 * 4 + 1, 0),
                Some(control),
                "{kind:?} hit area"
            );
            let (anchor, label) = tooltip_target(&app, control).expect("tooltip");
            assert_eq!(anchor, Rect::new(index as u16 * 4, 0, 3, 1));
            assert!(!label.trim().is_empty(), "{kind:?} tooltip empty");
        }
    }

    #[test]
    fn a_segment_that_elided_explains_nothing() {
        let app = AppState::test_new();
        assert!(tooltip_target(&app, ControlId::StatusSegment(StatusSegmentKind::Cpu)).is_none());
    }

    #[test]
    fn hit_areas_cover_the_drawn_row_right_aligned() {
        let mut app = AppState::test_new();
        let area = Rect::new(0, 0, 200, 1);
        app.view.status_segments = crate::ui::status::fitted_status_segments(&app, area);
        let areas = crate::ui::status::status_segment_hit_areas(&app, area);
        assert!(areas
            .iter()
            .any(|(kind, _)| *kind == StatusSegmentKind::Cpu));
        assert!(areas
            .windows(2)
            .all(|pair| pair[0].1.right() == pair[1].1.x));
        let reserved = crate::ui::tabs::tab_action_status_bar_reserved_width(&app, area);
        assert_eq!(
            areas.last().expect("segments").1.right(),
            area.width - reserved
        );
    }

    #[test]
    fn configured_machine_tooltip_lists_providers_and_marks_current_profile() {
        let mut app = AppState::test_new();
        app.machines.push(crate::app::machines::Machine {
            name: "workbox".into(),
            icon: None,
            target: Some("workbox".into()),
            socket: None,
        });
        app.launch_profiles =
            crate::app::launch_profiles::resolve(&[crate::config::LaunchProfileConfig {
                id: "codex".into(),
                label: "Codex".into(),
                agent: "codex".into(),
                command: Vec::new(),
                env: Default::default(),
                usage: Some("codex".into()),
            }]);
        app.next_home_profile = Some("codex".into());

        let tooltip = status_segment_tooltip(&app, StatusSegmentKind::FleetDevice(0));

        assert!(tooltip.contains("Antigravity"), "{tooltip}");
        assert!(tooltip.contains("Codex (current)"), "{tooltip}");
        assert!(tooltip.contains("OpenCode"), "{tooltip}");
        assert!(tooltip.contains("status unknown"), "{tooltip}");
    }
}
