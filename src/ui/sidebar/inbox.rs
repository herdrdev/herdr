//! Agent-inbox sidebar projection and compact row renderer.

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    section_is_collapsed, workspace_list_body_rect, workspace_list_rect_for_app, SidebarRow,
    INBOX_SECTION_TITLE,
};
use crate::app::AppState;
use crate::ui::scrollbar::should_show_scrollbar;

#[derive(Clone)]
pub(crate) enum InboxLine {
    Header { collapsed: bool },
    Switch,
    Columns,
    Source(usize),
    Fault(String),
}

pub(super) fn append_rows(app: &AppState, rows: &mut Vec<SidebarRow>) {
    let Some(snapshot) = app.fleet_snapshot.inbox.as_ref() else {
        return;
    };
    if !app.fleet_snapshot.polled {
        return;
    }
    let collapsed = section_is_collapsed(app, INBOX_SECTION_TITLE);
    rows.push(SidebarRow::Inbox(InboxLine::Header { collapsed }));
    if collapsed {
        return;
    }
    if let crate::inbox::ProducerState::Unreachable(_) = &snapshot.state {
        // Error details are hover-only; the row shows a generic marker.
        rows.push(SidebarRow::Inbox(InboxLine::Fault(
            "healthcheck failed".into(),
        )));
        return;
    }
    if !matches!(snapshot.state, crate::inbox::ProducerState::Read) {
        return;
    }
    rows.push(SidebarRow::Inbox(InboxLine::Switch));
    rows.push(SidebarRow::Inbox(InboxLine::Columns));
    let mut order = (0..snapshot.data.sources.len()).collect::<Vec<_>>();
    order.sort_by_key(|index| !snapshot.data.sources[*index].enabled);
    for index in order {
        rows.push(SidebarRow::Inbox(InboxLine::Source(index)));
    }
}

#[derive(Clone)]
pub(super) struct Area {
    pub(super) line: InboxLine,
    pub(super) rect: Rect,
}

pub(super) fn areas(app: &AppState, area: Rect) -> Vec<Area> {
    let ws = workspace_list_rect_for_app(app, area);
    let metrics = super::workspace_list_scroll_metrics(app, ws);
    let body = workspace_list_body_rect(app, ws, should_show_scrollbar(metrics));
    let rows = super::sidebar_rows(app);
    let mut y = body.y;
    let mut out = Vec::new();
    for (idx, row) in rows
        .iter()
        .enumerate()
        .skip(app.workspace_scroll.min(metrics.max_offset_from_bottom))
    {
        let height = super::sidebar_row_height(app, row, body.height);
        if y.saturating_add(height) > body.bottom() {
            break;
        }
        if let SidebarRow::Inbox(line) = row {
            out.push(Area {
                line: line.clone(),
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(super::sidebar_row_gap(app, &rows, idx));
    }
    out
}

pub(super) fn render(app: &AppState, frame: &mut Frame, area: &Area, now: std::time::SystemTime) {
    let Some(snapshot) = app.fleet_snapshot.inbox.as_ref() else {
        return;
    };
    let p = &app.palette;
    let dim = Style::default().fg(p.overlay0);
    let amber = Style::default().fg(p.yellow);
    let red = Style::default().fg(p.red);
    let mut spans = Vec::new();
    match &area.line {
        InboxLine::Header { collapsed } => {
            let unread = snapshot
                .data
                .sources
                .iter()
                .map(|source| source.undelivered)
                .sum::<u64>();
            let errors = snapshot
                .data
                .sources
                .iter()
                .filter(|source| source.error.is_some())
                .count();
            let enabled = snapshot.data.inbox_enabled;
            let section_fault = section_fault(snapshot, now);
            spans.push(Span::styled(
                if *collapsed { "▸ inbox" } else { "▾ inbox" },
                Style::default().fg(p.text).add_modifier(Modifier::BOLD),
            ));
            spans.push(Span::raw(" "));
            spans.push(Span::styled(
                if enabled { "● on" } else { "○ OFF" },
                if enabled {
                    Style::default().fg(p.green)
                } else {
                    dim
                },
            ));
            if errors > 0 {
                spans.push(Span::styled(format!("  ✗{errors}"), red));
            }
            if section_fault {
                spans.push(Span::styled("  ✗", red));
            }
            if *collapsed {
                spans.push(Span::styled(
                    format!("  {unread} new"),
                    if unread > 0 { amber } else { dim },
                ));
                let latest = snapshot
                    .data
                    .sources
                    .iter()
                    .filter_map(|source| {
                        source
                            .last_intake_time
                            .as_deref()
                            .and_then(crate::fleet::parse_utc_timestamp)
                    })
                    .max();
                spans.push(Span::styled(
                    format!(
                        "  {}",
                        latest.map_or_else(|| "never".into(), |at| age(now, at))
                    ),
                    if latest.is_none_or(|at| {
                        now.duration_since(
                            std::time::UNIX_EPOCH + std::time::Duration::from_secs(at),
                        )
                        .unwrap_or_default()
                        .as_secs()
                            > 86_400
                    }) {
                        amber
                    } else {
                        dim
                    },
                ));
            }
        }
        InboxLine::Switch => spans.extend([
            Span::raw("  "),
            Span::styled(
                if snapshot.data.inbox_enabled {
                    "● on"
                } else {
                    "○ OFF"
                },
                if snapshot.data.inbox_enabled {
                    Style::default().fg(p.green)
                } else {
                    dim
                },
            ),
            Span::raw("  "),
            Span::styled(
                format!("timer {}", snapshot.data.timer_state),
                if snapshot.data.timer_state == "active" {
                    dim
                } else {
                    red
                },
            ),
        ]),
        InboxLine::Columns => spans.push(Span::styled("  src      last    cur  new", dim)),
        InboxLine::Source(index) => {
            if let Some(source) = snapshot.data.sources.get(*index) {
                if source.error.is_some() {
                    spans.push(Span::styled(format!("  ✗ {}", source.name), red));
                } else {
                    let base = if source.enabled {
                        Style::default().fg(p.green)
                    } else {
                        dim
                    };
                    spans.push(Span::styled(
                        if source.enabled { "  ● " } else { "  ○ " },
                        base,
                    ));
                    spans.push(Span::styled(format!("{:<8}", source.name), base));
                    let last_at = source
                        .last_intake_time
                        .as_deref()
                        .and_then(crate::fleet::parse_utc_timestamp);
                    let last = last_at
                        .map(|at| age(now, at))
                        .or_else(|| (source.name != "whisper").then(|| "never".into()));
                    let cursor = source.cursor_age_s.map(age_seconds);
                    for (value, stale) in [
                        (
                            last.as_deref().unwrap_or("?"),
                            last_at.is_none() && source.name != "whisper"
                                || last_at.is_some_and(|at| {
                                    now.duration_since(
                                        std::time::UNIX_EPOCH + std::time::Duration::from_secs(at),
                                    )
                                    .unwrap_or_default()
                                    .as_secs()
                                        > 86_400
                                }),
                        ),
                        (
                            cursor.as_deref().unwrap_or("?"),
                            source.cursor_age_s.is_some_and(|seconds| seconds > 86_400),
                        ),
                    ] {
                        spans.push(Span::styled(
                            format!(" {:>6}", value),
                            if stale { amber } else { dim },
                        ));
                    }
                    spans.push(Span::styled(
                        format!(" {:>3}", source.undelivered),
                        if source.undelivered > 0 { amber } else { base },
                    ));
                }
            }
        }
        InboxLine::Fault(fault) => spans.push(Span::styled(format!("  ✗ {fault}"), red)),
    }
    frame.render_widget(Paragraph::new(Line::from(spans)), area.rect);
}

pub(super) fn hover_detail(
    app: &AppState,
    line: &InboxLine,
    now: std::time::SystemTime,
) -> Option<String> {
    let snapshot = app.fleet_snapshot.inbox.as_ref()?;
    match line {
        InboxLine::Source(index) => snapshot.data.sources.get(*index)?.error.clone(),
        InboxLine::Header { .. } | InboxLine::Fault(_) if section_fault(snapshot, now) => {
            let mut issues = Vec::new();
            match &snapshot.state {
                crate::inbox::ProducerState::Unreachable(error) => {
                    issues.push(format!("healthcheck failed: {error}"))
                }
                crate::inbox::ProducerState::Unconfigured => {
                    issues.push("producer host not configured".into())
                }
                crate::inbox::ProducerState::Read => {}
            }
            if snapshot.data.inbox_enabled && snapshot.data.timer_state != "active" {
                issues.push(format!("timer {}", snapshot.data.timer_state));
            }
            if !snapshot.data.surface_reachable {
                issues.push("surface unreachable".into());
            }
            if is_stale(snapshot, now) {
                issues.push("healthcheck stale".into());
            }
            Some(issues.join("; "))
        }
        _ => None,
    }
}

fn section_fault(snapshot: &crate::inbox::ProducerSnapshot, now: std::time::SystemTime) -> bool {
    !matches!(snapshot.state, crate::inbox::ProducerState::Read)
        || (snapshot.data.inbox_enabled && snapshot.data.timer_state != "active")
        || !snapshot.data.surface_reachable
        || is_stale(snapshot, now)
}

fn is_stale(snapshot: &crate::inbox::ProducerSnapshot, now: std::time::SystemTime) -> bool {
    snapshot.refreshed_at.is_some_and(|refreshed| {
        now.duration_since(refreshed).unwrap_or_default() > std::time::Duration::from_secs(60)
    })
}

fn age(now: std::time::SystemTime, at: u64) -> String {
    let seconds = now
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs()
        .saturating_sub(at);
    age_seconds(seconds)
}

fn age_seconds(seconds: u64) -> String {
    if seconds >= 86_400 {
        format!("{}d{}h", seconds / 86_400, seconds % 86_400 / 3_600)
    } else if seconds >= 3_600 {
        format!("{}h{}m", seconds / 3_600, seconds % 3_600 / 60)
    } else if seconds >= 60 {
        format!("{}m", seconds / 60)
    } else {
        format!("{}s", seconds)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn source(enabled: bool, error: Option<&str>) -> crate::inbox::Source {
        crate::inbox::Source {
            name: "box".into(),
            enabled,
            last_intake_time: Some("2026-10-01T06:00:00Z".into()),
            cursor_age_s: Some(60),
            undelivered: 2,
            error: error.map(str::to_owned),
        }
    }

    fn rendered(
        data: crate::inbox::Healthcheck,
        line: InboxLine,
        state: crate::inbox::ProducerState,
    ) -> String {
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_836_800);
        let mut app = AppState::test_new();
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot {
            host: "ub2".into(),
            state,
            data,
            refreshed_at: Some(now),
        }));
        let backend = TestBackend::new(100, 1);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render(
                    &app,
                    frame,
                    &Area {
                        line,
                        rect: Rect::new(0, 0, 100, 1),
                    },
                    now,
                )
            })
            .expect("draw");
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>()
            .trim_end()
            .to_owned()
    }

    #[test]
    fn collapsed_render_snapshot_includes_switch_errors_and_latest_age() {
        let data = crate::inbox::Healthcheck {
            inbox_enabled: true,
            timer_state: "active".into(),
            surface_reachable: true,
            sources: vec![source(true, Some("auth 401"))],
        };
        let text = rendered(
            data,
            InboxLine::Header { collapsed: true },
            crate::inbox::ProducerState::Read,
        );
        assert!(text.contains("▸ inbox ● on  ✗1"));
        assert!(text.contains("2 new"));
        assert!(text.contains("40m"));
    }

    #[test]
    fn expanded_off_and_source_error_render_snapshots_stay_compact() {
        let data = crate::inbox::Healthcheck {
            inbox_enabled: false,
            timer_state: "active".into(),
            surface_reachable: true,
            sources: vec![source(false, None)],
        };
        let header = rendered(
            data.clone(),
            InboxLine::Header { collapsed: false },
            crate::inbox::ProducerState::Read,
        );
        assert!(header.contains("▾ inbox ○ OFF"));
        let mut enabled = data;
        enabled.sources[0].enabled = true;
        let row = rendered(
            enabled,
            InboxLine::Source(0),
            crate::inbox::ProducerState::Read,
        );
        assert!(row.contains("● box"));
        assert!(row.contains("1m"));
    }

    #[test]
    fn source_error_render_has_no_inline_reason() {
        let data = crate::inbox::Healthcheck {
            inbox_enabled: true,
            timer_state: "active".into(),
            surface_reachable: true,
            sources: vec![source(true, Some("cred missing"))],
        };
        let text = rendered(
            data,
            InboxLine::Source(0),
            crate::inbox::ProducerState::Read,
        );
        assert!(text.contains("✗ box"));
        assert!(!text.contains("cred missing"));
    }

    #[test]
    fn source_error_detail_is_only_a_hover_target() {
        let data = crate::inbox::Healthcheck {
            inbox_enabled: true,
            timer_state: "active".into(),
            surface_reachable: true,
            sources: vec![source(true, Some("cred missing"))],
        };
        let now = std::time::UNIX_EPOCH + std::time::Duration::from_secs(1_790_836_800);
        let mut app = AppState::test_new();
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot {
            host: "ub2".into(),
            state: crate::inbox::ProducerState::Read,
            data,
            refreshed_at: Some(now),
        }));
        assert_eq!(
            hover_detail(&app, &InboxLine::Source(0), now).as_deref(),
            Some("cred missing")
        );
    }

    #[test]
    fn section_faults_collapse_to_a_header_marker_and_hover_detail() {
        let data = crate::inbox::Healthcheck {
            inbox_enabled: true,
            timer_state: "failed".into(),
            surface_reachable: false,
            sources: Vec::new(),
        };
        let now = std::time::SystemTime::now();
        let text = rendered(
            data.clone(),
            InboxLine::Header { collapsed: false },
            crate::inbox::ProducerState::Read,
        );
        assert!(text.contains("▾ inbox ● on  ✗"));
        let mut app = AppState::test_new();
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot {
            host: "ub2".into(),
            state: crate::inbox::ProducerState::Read,
            data,
            refreshed_at: Some(now),
        }));
        assert_eq!(
            hover_detail(&app, &InboxLine::Header { collapsed: false }, now).as_deref(),
            Some("timer failed; surface unreachable")
        );
    }

    #[test]
    fn collapse_uses_the_shared_persisted_sidebar_group_setting() {
        let mut app = AppState::test_new();
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot::read(
            "ub2".into(),
            crate::inbox::Healthcheck::default(),
            std::time::SystemTime::now(),
        )));
        assert!(app.collapsed_sidebar_groups.contains("repo:Inbox"));
        app.toggle_sidebar_group(crate::ui::sidebar::INBOX_SECTION_TITLE);
        assert!(!app.collapsed_sidebar_groups.contains("repo:Inbox"));
        assert_eq!(
            app.take_sidebar_group_collapsed_persistence_request(),
            Some(("repo:Inbox".into(), false))
        );
    }

    #[test]
    fn unreachable_producer_detail_is_only_a_hover_target() {
        let now = std::time::SystemTime::now();
        let mut app = AppState::test_new();
        app.fleet_snapshot.polled = true;
        app.fleet_snapshot.inbox = Some(std::sync::Arc::new(crate::inbox::ProducerSnapshot {
            host: "ub2".into(),
            state: crate::inbox::ProducerState::Unreachable("connection refused".into()),
            data: crate::inbox::Healthcheck::default(),
            refreshed_at: Some(now),
        }));
        app.toggle_sidebar_group(crate::ui::sidebar::INBOX_SECTION_TITLE);
        let mut rows = Vec::new();
        append_rows(&app, &mut rows);
        let fault = rows
            .iter()
            .find_map(|row| match row {
                SidebarRow::Inbox(line @ InboxLine::Fault(_)) => Some(line.clone()),
                _ => None,
            })
            .expect("fault row");
        let InboxLine::Fault(label) = &fault else {
            unreachable!("matched a fault row")
        };
        assert!(!label.contains("connection refused"));
        let text = rendered(
            crate::inbox::Healthcheck::default(),
            fault.clone(),
            crate::inbox::ProducerState::Unreachable("connection refused".into()),
        );
        assert!(text.contains("healthcheck failed"));
        assert!(!text.contains("connection refused"));
        assert!(hover_detail(&app, &fault, now)
            .is_some_and(|detail| detail.contains("connection refused")));
    }
}
