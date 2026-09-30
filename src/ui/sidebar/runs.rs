use std::sync::Arc;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    compact_dot_for_state, compact_row_widths, pad_left, pad_right, section_is_collapsed,
    short_fleet_host_names, sidebar_row_gap, sidebar_row_height, sidebar_rows,
    workspace_list_body_rect, workspace_list_rect_for_app, SidebarHostCount, SidebarRow,
    RUNS_SECTION_TITLE, SIDEBAR_AGE_FIELD_WIDTH, SIDEBAR_DOT_FIELD_WIDTH,
};
use crate::app::AppState;
use crate::detect::AgentState;
use crate::ui::scrollbar::should_show_scrollbar;
use crate::ui::status::state_label_color;
use crate::ui::text::truncate_end;

const ROW_DEPTH: usize = 2;

pub(super) fn append_rows(app: &AppState, rows: &mut Vec<SidebarRow>) {
    if !app.fleet_snapshot.polled {
        return;
    }
    let mut projection = crate::agent_runs::project(&app.fleet_snapshot);
    if app.sidebar_work_filter.machine_scope == crate::app::state::SidebarMachineScope::ThisMachine
    {
        projection
            .hosts
            .retain(|host| host.name == app.agent_host_name);
        projection.active_count = projection.hosts.iter().map(|host| host.active_count).sum();
    }
    if projection.hosts.is_empty() {
        return;
    }
    let host_tokens =
        short_fleet_host_names(projection.hosts.iter().map(|host| host.name.as_str()));
    let collapsed = section_is_collapsed(app, RUNS_SECTION_TITLE);
    rows.push(SidebarRow::SectionHeader {
        title: RUNS_SECTION_TITLE,
        count: projection.active_count,
        host_counts: projection
            .hosts
            .iter()
            .filter(|host| host.active_count > 0)
            .map(|host| SidebarHostCount {
                host: host_tokens
                    .get(&host.name)
                    .cloned()
                    .expect("host token for Runs host"),
                count: host.active_count,
            })
            .collect(),
        collapsed,
    });
    if collapsed {
        return;
    }
    for host in projection.hosts {
        let key = format!("runs:host:{}", host.name);
        let collapsed = section_is_collapsed(app, &key);
        rows.push(SidebarRow::NestedHeader {
            key,
            action_key: None,
            sort_key: None,
            sort_mode: crate::app::state::SidebarSortMode::Default,
            title: host_tokens
                .get(&host.name)
                .cloned()
                .expect("host token for Runs host"),
            count: host.active_count,
            activity_count: None,
            collapsed,
            dim: false,
            status: None,
            spawn: false,
        });
        if collapsed {
            continue;
        }
        if host.runs.is_empty() {
            rows.push(SidebarRow::AgentRun {
                host: host.name,
                summary: None,
            });
        } else {
            rows.extend(host.runs.into_iter().map(|summary| SidebarRow::AgentRun {
                host: host.name.clone(),
                summary: Some(summary),
            }));
        }
    }
}

#[derive(Clone)]
pub(super) struct Area {
    pub(super) host: String,
    pub(super) summary: Option<Arc<crate::agent_runs::Summary>>,
    pub(super) rect: Rect,
}

pub(super) fn areas(app: &AppState, area: Rect) -> Vec<Area> {
    let ws_area = workspace_list_rect_for_app(app, area);
    let metrics = super::workspace_list_scroll_metrics(app, ws_area);
    let body = workspace_list_body_rect(app, ws_area, should_show_scrollbar(metrics));
    let rows = sidebar_rows(app);
    let mut y = body.y;
    let mut out = Vec::new();
    for (idx, row) in rows
        .iter()
        .enumerate()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
    {
        let height = sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        if let SidebarRow::AgentRun { host, summary } = row {
            out.push(Area {
                host: host.clone(),
                summary: summary.clone(),
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }
    out
}

pub(super) fn target_at(app: &AppState, row: u16) -> Option<(String, String)> {
    areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|area| row >= area.rect.y && row < area.rect.bottom())
        .and_then(|area| {
            area.summary
                .map(|summary| (area.host, summary.run_id.clone()))
        })
}

pub(super) fn render(app: &AppState, frame: &mut Frame, area: &Area, now: std::time::SystemTime) {
    if area.rect.width == 0 || area.rect.height == 0 {
        return;
    }
    let Some(summary) = area.summary.as_ref() else {
        let line = format!("{}no recent runs", " ".repeat(ROW_DEPTH * 2));
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                truncate_end(&line, usize::from(area.rect.width)),
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ))),
            area.rect,
        );
        return;
    };
    let state = match summary.state {
        crate::agent_runs::DisplayState::Active => AgentState::Working,
        crate::agent_runs::DisplayState::Blocked => AgentState::Blocked,
        crate::agent_runs::DisplayState::Stale => AgentState::Unknown,
        crate::agent_runs::DisplayState::Done
        | crate::agent_runs::DisplayState::Failed
        | crate::agent_runs::DisplayState::Empty => AgentState::Idle,
    };
    let status = match summary.state {
        crate::agent_runs::DisplayState::Stale => "stale".to_string(),
        crate::agent_runs::DisplayState::Done => "done".to_string(),
        crate::agent_runs::DisplayState::Failed => "failed".to_string(),
        crate::agent_runs::DisplayState::Empty => "empty".to_string(),
        crate::agent_runs::DisplayState::Blocked if summary.phase.trim().is_empty() => {
            "blocked".to_string()
        }
        crate::agent_runs::DisplayState::Blocked => format!("blocked:{}", summary.phase),
        crate::agent_runs::DisplayState::Active if summary.phase.trim().is_empty() => {
            "active".to_string()
        }
        crate::agent_runs::DisplayState::Active => summary.phase.clone(),
    };
    let title = if !summary.label.trim().is_empty() {
        summary.label.as_str()
    } else if !summary.task.trim().is_empty() {
        summary.task.as_str()
    } else {
        summary.run_id.as_str()
    };
    let width = usize::from(area.rect.width);
    let widths = compact_row_widths(title, &status, width, ROW_DEPTH * 3 + 1);
    let fixed = widths.prefix + SIDEBAR_DOT_FIELD_WIDTH + widths.provider + widths.age;
    let title_width = width.saturating_sub(fixed);
    let age = if widths.age == SIDEBAR_AGE_FIELD_WIDTH {
        crate::ui::symphony::age_label_since(Some(summary.started_at.as_str()), now)
    } else {
        String::new()
    };
    frame.render_widget(
        Paragraph::new(Line::from(vec![
            Span::raw(" ".repeat(widths.prefix)),
            Span::styled(
                pad_right(
                    compact_dot_for_state(state, true, true, false, false),
                    SIDEBAR_DOT_FIELD_WIDTH,
                ),
                Style::default().fg(state_label_color(state, true, &app.palette)),
            ),
            Span::styled(
                pad_right(&truncate_end(title, title_width), title_width),
                Style::default().fg(app.palette.subtext0),
            ),
            Span::styled(
                pad_left(&status, widths.provider),
                Style::default()
                    .fg(app.palette.overlay0)
                    .add_modifier(Modifier::DIM),
            ),
            Span::styled(
                pad_left(&age, widths.age),
                Style::default().fg(app.palette.overlay0),
            ),
        ])),
        area.rect,
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn host(state: crate::fleet::HostState) -> crate::fleet::HostSnapshot {
        crate::fleet::HostSnapshot {
            name: "ub2".to_string(),
            target: "ub2".to_string(),
            local: false,
            session: None,
            socket: None,
            state,
            version: None,
            protocol: None,
            error: (state == crate::fleet::HostState::Unreachable).then(|| "timeout".to_string()),
            remote_identity: None,
            sessions: None,
            reachable: false,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }
    }

    #[test]
    fn reachable_empty_host_has_runs_empty_state() {
        let mut app = AppState::test_new();
        app.collapsed_sidebar_groups.remove("repo:Runs");
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.hosts = vec![host(crate::fleet::HostState::Reachable)];

        let rows = super::super::sidebar_rows(&app);
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::SectionHeader { title, count: 0, .. }
                if *title == RUNS_SECTION_TITLE
        )));
        assert!(rows
            .iter()
            .any(|row| matches!(row, SidebarRow::AgentRun { summary: None, .. })));
    }

    #[test]
    fn unreachable_host_does_not_claim_known_run_state() {
        let mut app = AppState::test_new();
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.hosts = vec![host(crate::fleet::HostState::Unreachable)];

        assert!(super::super::sidebar_rows(&app).iter().all(|row| !matches!(
            row,
            SidebarRow::SectionHeader { title, .. } if *title == RUNS_SECTION_TITLE
        )));
    }

    #[test]
    fn run_row_keeps_phase_and_title_at_normal_width_and_survives_narrow_width() {
        let summary = Arc::new(crate::agent_runs::Summary {
            host: "ub2".to_string(),
            run_id: "ra-sidebar".to_string(),
            label: "runs sidebar".to_string(),
            task: "Add active runs per machine".to_string(),
            phase: "verify".to_string(),
            started_at: "2026-09-17T08:00:00Z".to_string(),
            started_at_unix_s: 1_779_000_000,
            heartbeat_age_s: Some(1),
            state: crate::agent_runs::DisplayState::Active,
        });
        let app = AppState::test_new();

        for width in [18, 60] {
            let mut terminal = Terminal::new(TestBackend::new(width, 1)).expect("terminal");
            terminal
                .draw(|frame| {
                    render(
                        &app,
                        frame,
                        &Area {
                            host: "ub2".to_string(),
                            summary: Some(Arc::clone(&summary)),
                            rect: Rect::new(0, 0, width, 1),
                        },
                        std::time::SystemTime::UNIX_EPOCH
                            + std::time::Duration::from_secs(1_779_000_060),
                    );
                })
                .expect("draw run row");
            let rendered = (0..width)
                .map(|x| terminal.backend().buffer()[(x, 0)].symbol())
                .collect::<String>();
            assert!(!rendered.trim().is_empty(), "width {width}");
            if width == 60 {
                assert!(rendered.contains("runs sidebar"), "{rendered:?}");
                assert!(rendered.contains("verify"), "{rendered:?}");
            }
        }
    }
}
