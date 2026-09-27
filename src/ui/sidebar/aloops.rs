//! The Aloops sidebar section (MAT-159): pending findings and run history
//! from the producer host, read out of the fleet snapshot. Rows are rebuilt
//! from the projection on every pass exactly like the Runs section; the
//! section never starts an agent on its own (AC5).

use std::sync::Arc;

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use super::{
    pad_left, pad_right, section_is_collapsed, sidebar_row_gap, sidebar_row_height, sidebar_rows,
    workspace_list_body_rect, workspace_list_rect_for_app, SidebarRow, ALOOPS_SECTION_TITLE,
    SIDEBAR_AGE_FIELD_WIDTH,
};
use crate::app::AppState;
use crate::ui::scrollbar::should_show_scrollbar;
use crate::ui::text::{display_width, truncate_end};

/// Loop header rows fold through the shared collapse sets under this key
/// prefix; the same key selects the row for keyboard activation.
pub(crate) const ALOOP_LOOP_KEY_PREFIX: &str = "aloop:loop:";
/// Hit-run rows: expanded by default, folding stores the key in the collapsed
/// set.
pub(crate) const ALOOP_RUN_KEY_PREFIX: &str = "aloop:run:";
/// The clean-runs summary line: folded by default, expanding stores the key
/// in the expanded set (same rule as `host:` groups).
pub(crate) const ALOOP_CLEAN_KEY_PREFIX: &str = "aloop-clean:";
pub(crate) const ALOOP_FINDING_KEY_PREFIX: &str = "aloop:finding:";
pub(crate) const ALOOP_CLEAN_RUN_KEY_PREFIX: &str = "aloop:cleanrun:";

pub(crate) fn finding_key(finding: &crate::aloop::Finding) -> String {
    format!(
        "{ALOOP_FINDING_KEY_PREFIX}{}:{}",
        finding.loop_name, finding.stable_id
    )
}

pub(super) fn append_rows(app: &AppState, rows: &mut Vec<SidebarRow>) {
    if !app.fleet_snapshot.polled {
        return;
    }
    let Some(projection) = app.aloop_projection() else {
        return;
    };
    let collapsed = section_is_collapsed(app, ALOOPS_SECTION_TITLE);
    rows.push(SidebarRow::SectionHeader {
        title: ALOOPS_SECTION_TITLE,
        count: projection.pending_count,
        // The Aloops section reads one producer host, so it carries no
        // per-host breakdown the way the Fleet section does.
        host_counts: Vec::new(),
        collapsed,
    });
    if collapsed {
        return;
    }
    if !projection.reachable {
        rows.push(SidebarRow::AloopUnreachable {
            host: projection.host.clone(),
            error: projection.error.clone(),
        });
        return;
    }
    if projection.pending_count == 0 {
        rows.push(SidebarRow::AloopEmpty);
    }
    for loop_projection in &projection.loops {
        let key = format!("{ALOOP_LOOP_KEY_PREFIX}{}", loop_projection.name);
        let collapsed = section_is_collapsed(app, &key);
        let pending = loop_projection.unlinked_findings.len()
            + loop_projection
                .runs
                .iter()
                .map(|run| run.pending.len())
                .sum::<usize>();
        rows.push(SidebarRow::AloopLoop {
            key,
            name: loop_projection.name.clone(),
            state: loop_projection.state,
            interval: loop_projection.interval.clone(),
            host: projection.host.clone(),
            collapsed,
            pending,
        });
        if collapsed {
            continue;
        }
        for finding in &loop_projection.unlinked_findings {
            rows.push(SidebarRow::AloopFinding {
                key: finding_key(finding),
                finding: Arc::clone(finding),
            });
        }
        for run_projection in &loop_projection.runs {
            let key = format!(
                "{ALOOP_RUN_KEY_PREFIX}{}:{}",
                loop_projection.name, run_projection.run.at
            );
            let expanded = !section_is_collapsed(app, &key);
            rows.push(SidebarRow::AloopRunLine {
                key,
                run: Arc::clone(&run_projection.run),
                pending: run_projection.run.findings as usize,
                expanded,
            });
            if expanded {
                for finding in &run_projection.pending {
                    rows.push(SidebarRow::AloopFinding {
                        key: finding_key(finding),
                        finding: Arc::clone(finding),
                    });
                }
            }
        }
        if !loop_projection.clean_runs.is_empty() {
            let key = format!("{ALOOP_CLEAN_KEY_PREFIX}{}", loop_projection.name);
            let expanded = !section_is_collapsed(app, &key);
            rows.push(SidebarRow::AloopCleanRuns {
                key,
                count: loop_projection.clean_runs.len(),
                last_at: loop_projection.clean_runs[0].at.clone(),
                expanded,
            });
            if expanded {
                for run in &loop_projection.clean_runs {
                    rows.push(SidebarRow::AloopCleanRun {
                        key: format!(
                            "{ALOOP_CLEAN_RUN_KEY_PREFIX}{}:{}",
                            loop_projection.name, run.at
                        ),
                        loop_name: loop_projection.name.clone(),
                        run: Arc::clone(run),
                    });
                }
            }
        }
    }
}

#[derive(Clone)]
pub(super) struct Area {
    pub(super) row: SidebarRow,
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
        if matches!(
            row,
            SidebarRow::AloopLoop { .. }
                | SidebarRow::AloopRunLine { .. }
                | SidebarRow::AloopFinding { .. }
                | SidebarRow::AloopCleanRuns { .. }
                | SidebarRow::AloopCleanRun { .. }
                | SidebarRow::AloopUnreachable { .. }
                | SidebarRow::AloopEmpty
        ) {
            out.push(Area {
                row: row.clone(),
                rect: Rect::new(body.x, y, body.width, height),
            });
        }
        y = y
            .saturating_add(height)
            .saturating_add(sidebar_row_gap(app, &rows, idx));
    }
    out
}

/// What a click on an Aloops row means. `fold: true` on `Loop` marks the
/// glyph cell: the header opens the run-history table, its glyph folds.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum AloopTarget {
    Finding {
        key: String,
    },
    Loop {
        key: String,
        name: String,
        fold: bool,
    },
    RunLine {
        key: String,
    },
    CleanFold {
        key: String,
    },
    CleanRun {
        key: String,
        loop_name: String,
        at: String,
    },
}

pub(super) fn target_at(app: &AppState, col: u16, row: u16) -> Option<AloopTarget> {
    let area = areas(app, app.view.sidebar_rect)
        .into_iter()
        .find(|area| row >= area.rect.y && row < area.rect.bottom())?;
    let left_edge = area.rect.x;
    match area.row {
        SidebarRow::AloopFinding { key, .. } => Some(AloopTarget::Finding { key }),
        SidebarRow::AloopLoop { key, name, .. } => Some(AloopTarget::Loop {
            key,
            name,
            // The fold glyph sits two columns in, matching the row prefix.
            fold: col <= left_edge.saturating_add(2),
        }),
        SidebarRow::AloopRunLine { key, .. } => Some(AloopTarget::RunLine { key }),
        SidebarRow::AloopCleanRuns { key, .. } => Some(AloopTarget::CleanFold { key }),
        SidebarRow::AloopCleanRun {
            key,
            loop_name,
            run,
        } => Some(AloopTarget::CleanRun {
            key,
            loop_name,
            at: run.at.clone(),
        }),
        _ => None,
    }
}

fn duration_label(duration_ms: u64) -> String {
    if duration_ms >= 60_000 {
        format!("{}m", duration_ms / 60_000)
    } else {
        format!("{:.1}s", duration_ms as f64 / 1_000.0)
    }
}

/// `HH:MM` out of a validated RFC3339 timestamp.
fn clock_label(at: &str) -> &str {
    at.get(11..16).unwrap_or(at)
}

fn selected_style(app: &AppState, key: &str) -> Style {
    if app.sidebar_selected_work_group.as_deref() == Some(key) {
        Style::default().bg(app.palette.surface1)
    } else {
        Style::default()
    }
}

pub(super) fn render(app: &AppState, frame: &mut Frame, area: &Area, now: std::time::SystemTime) {
    if area.rect.width == 0 || area.rect.height == 0 {
        return;
    }
    let p = &app.palette;
    let width = usize::from(area.rect.width);
    let line = match &area.row {
        SidebarRow::AloopLoop {
            key,
            name,
            state,
            interval,
            host,
            collapsed,
            pending,
        } => {
            let mut spans = vec![
                Span::raw("  "),
                Span::styled(
                    if *collapsed { "▸" } else { "▾" },
                    Style::default().fg(p.overlay0),
                ),
                Span::raw(" "),
                Span::styled(
                    truncate_end(name, width.saturating_sub(4)),
                    Style::default().fg(p.text).add_modifier(Modifier::BOLD),
                ),
            ];
            let mut tail = String::new();
            if let Some(state) = state {
                tail.push_str(&format!(" · {}", state.label()));
            }
            if let Some(interval) = interval {
                tail.push_str(&format!(" · every {interval}"));
            }
            tail.push_str(&format!(" · on {host}"));
            if *pending > 0 {
                tail.push_str(&format!(" · {pending} pending"));
            }
            spans.push(Span::styled(
                truncate_end(&tail, width.saturating_sub(4 + name.len())),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            ));
            Paragraph::new(Line::from(spans)).style(selected_style(app, key))
        }
        SidebarRow::AloopRunLine {
            key,
            run,
            pending,
            expanded,
        } => {
            let findings = if *pending == 1 {
                "1 finding".to_string()
            } else {
                format!("{pending} findings")
            };
            let text = format!(
                "    {} {} · {} · {}",
                if *expanded { "▾" } else { "▸" },
                clock_label(&run.at),
                findings,
                duration_label(run.duration_ms)
            );
            Paragraph::new(Line::from(Span::styled(
                truncate_end(&text, width),
                Style::default().fg(p.yellow),
            )))
            .style(selected_style(app, key))
        }
        SidebarRow::AloopFinding { key, finding } => {
            let tag = format!("[{}]", finding.source);
            let age = crate::ui::symphony::age_label_since(Some(finding.created_at.as_str()), now);
            let prefix = 6;
            let tag_width = display_width(&tag);
            let age_width = if width >= prefix + tag_width + 1 + 8 + SIDEBAR_AGE_FIELD_WIDTH {
                SIDEBAR_AGE_FIELD_WIDTH
            } else {
                0
            };
            let title_width = width.saturating_sub(prefix + tag_width + 1 + age_width);
            Paragraph::new(Line::from(vec![
                Span::raw(" ".repeat(prefix.min(width))),
                Span::styled(
                    pad_right(
                        &truncate_end(&tag, tag_width.min(width.saturating_sub(prefix))),
                        tag_width.min(width.saturating_sub(prefix)),
                    ),
                    Style::default().fg(p.blue),
                ),
                Span::raw(" "),
                Span::styled(
                    pad_right(&truncate_end(&finding.title, title_width), title_width),
                    Style::default().fg(p.subtext0),
                ),
                Span::styled(pad_left(&age, age_width), Style::default().fg(p.overlay0)),
            ]))
            .style(selected_style(app, key))
        }
        SidebarRow::AloopCleanRuns {
            key,
            count,
            last_at,
            expanded,
        } => {
            let age = crate::ui::symphony::age_label_since(Some(last_at.as_str()), now);
            let text = format!(
                "    {} {} clean runs · last {} ago",
                if *expanded { "▾" } else { "▸" },
                count,
                age
            );
            Paragraph::new(Line::from(Span::styled(
                truncate_end(&text, width),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            )))
            .style(selected_style(app, key))
        }
        SidebarRow::AloopCleanRun { key, run, .. } => {
            let text = format!(
                "      {} · {} · 0 findings",
                clock_label(&run.at),
                duration_label(run.duration_ms)
            );
            Paragraph::new(Line::from(Span::styled(
                truncate_end(&text, width),
                Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
            )))
            .style(selected_style(app, key))
        }
        SidebarRow::AloopUnreachable { host, error } => {
            let mut spans = vec![Span::styled(
                truncate_end(&format!("  {host} unreachable"), width),
                Style::default().fg(p.red),
            )];
            if let Some(error) = error {
                spans.push(Span::styled(
                    truncate_end(
                        &format!(" · {error}"),
                        width.saturating_sub(2 + host.len() + 13),
                    ),
                    Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
                ));
            }
            Paragraph::new(Line::from(spans))
        }
        SidebarRow::AloopEmpty => Paragraph::new(Line::from(Span::styled(
            truncate_end("  no pending findings", width),
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        ))),
        _ => return,
    };
    frame.render_widget(line, area.rect);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::aloop::{Finding, FindingStatus, HostData, LoopRuns, ProducerSnapshot, RunRecord};

    fn finding(loop_name: &str, stable_id: &str, created_at: &str) -> Arc<Finding> {
        Arc::new(Finding {
            loop_name: loop_name.to_string(),
            source: "sentry".to_string(),
            stable_id: stable_id.to_string(),
            title: format!("boom {stable_id}"),
            url: None,
            evidence: "stacktrace".to_string(),
            prompt: "fix it".to_string(),
            created_at: created_at.to_string(),
            created_at_unix_s: crate::fleet::parse_utc_timestamp(created_at).expect("timestamp"),
            status: FindingStatus::Pending,
        })
    }

    fn run(at: &str, findings: u32, stable_ids: &[&str]) -> Arc<RunRecord> {
        Arc::new(RunRecord {
            at: at.to_string(),
            at_unix_s: crate::fleet::parse_utc_timestamp(at).expect("timestamp"),
            duration_ms: 1_400,
            exit: 0,
            findings,
            stable_ids: stable_ids.iter().map(|id| id.to_string()).collect(),
            log_excerpt: "log tail".to_string(),
        })
    }

    fn app_with_producer(producer: ProducerSnapshot) -> AppState {
        let mut app = AppState::test_new();
        app.collapsed_sidebar_groups.remove("repo:Aloops");
        app.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            aloop: Some(producer),
            ..Default::default()
        };
        app
    }

    fn aloop_rows(app: &AppState) -> Vec<SidebarRow> {
        sidebar_rows(app)
            .into_iter()
            .filter(|row| {
                matches!(
                    row,
                    SidebarRow::AloopLoop { .. }
                        | SidebarRow::AloopRunLine { .. }
                        | SidebarRow::AloopFinding { .. }
                        | SidebarRow::AloopCleanRuns { .. }
                        | SidebarRow::AloopCleanRun { .. }
                        | SidebarRow::AloopUnreachable { .. }
                        | SidebarRow::AloopEmpty
                )
            })
            .collect()
    }

    fn section_header(app: &AppState) -> Option<(usize, bool)> {
        sidebar_rows(app).into_iter().find_map(|row| match row {
            SidebarRow::SectionHeader {
                title,
                count,
                collapsed,
                ..
            } if title == ALOOPS_SECTION_TITLE => Some((count, collapsed)),
            _ => None,
        })
    }

    #[test]
    fn ac1_ac3_section_header_counts_pending_and_lists_findings() {
        let app = app_with_producer(ProducerSnapshot::read(
            "ub2".to_string(),
            HostData {
                findings: vec![
                    finding("nightly", "b2", "2026-09-18T09:52:00Z"),
                    finding("nightly", "a1", "2026-09-18T09:50:00Z"),
                ],
                ..Default::default()
            },
        ));

        assert_eq!(section_header(&app), Some((2, false)));
        let rows = aloop_rows(&app);
        let keys: Vec<&str> = rows
            .iter()
            .map(|row| match row {
                SidebarRow::AloopLoop { key, .. } => key.as_str(),
                SidebarRow::AloopFinding { key, .. } => key.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(
            keys,
            vec![
                "aloop:loop:nightly",
                // AC2: newest created_at first.
                "aloop:finding:nightly:b2",
                "aloop:finding:nightly:a1",
            ]
        );
    }

    #[test]
    fn ac6_unreachable_producer_renders_one_line_without_panicking() {
        let app = app_with_producer(ProducerSnapshot::unreachable(
            "ub2".to_string(),
            "ssh failed".to_string(),
        ));
        let rows = aloop_rows(&app);
        assert_eq!(rows.len(), 1);
        assert!(matches!(
            &rows[0],
            SidebarRow::AloopUnreachable { host, error }
                if host == "ub2" && error.as_deref() == Some("ssh failed")
        ));
    }

    #[test]
    fn ac6_zero_pending_renders_the_empty_line() {
        let app = app_with_producer(ProducerSnapshot::read(
            "ub2".to_string(),
            HostData::default(),
        ));
        assert_eq!(section_header(&app), Some((0, false)));
        let rows = aloop_rows(&app);
        assert_eq!(rows.len(), 1);
        assert!(matches!(rows[0], SidebarRow::AloopEmpty));
    }

    #[test]
    fn ac7_hit_runs_expand_and_clean_runs_fold_by_default() {
        let app = app_with_producer(ProducerSnapshot::read(
            "ub2".to_string(),
            HostData {
                findings: vec![finding("nightly", "a1", "2026-09-18T09:50:00Z")],
                loops: vec![LoopRuns {
                    loop_name: "nightly".to_string(),
                    runs: vec![
                        run("2026-09-18T09:59:00Z", 0, &[]),
                        run("2026-09-18T09:58:00Z", 0, &[]),
                        run("2026-09-18T09:50:00Z", 3, &["a1"]),
                    ],
                    skipped_lines: 0,
                }],
                ..Default::default()
            },
        ));

        let rows = aloop_rows(&app);
        // Hit run line expanded with its pending finding; clean runs folded
        // into one line.
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::AloopRunLine {
                expanded: true,
                pending: 3,
                ..
            }
        )));
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::AloopFinding { finding, .. } if finding.stable_id == "a1"
        )));
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::AloopCleanRuns {
                expanded: false,
                count: 2,
                ..
            }
        )));
        assert!(!rows
            .iter()
            .any(|row| matches!(row, SidebarRow::AloopCleanRun { .. })));
    }

    #[test]
    fn ac7_clean_run_fold_expands_into_log_rows() {
        let mut app = app_with_producer(ProducerSnapshot::read(
            "ub2".to_string(),
            HostData {
                loops: vec![LoopRuns {
                    loop_name: "nightly".to_string(),
                    runs: vec![run("2026-09-18T09:59:00Z", 0, &[])],
                    skipped_lines: 0,
                }],
                ..Default::default()
            },
        ));
        app.toggle_sidebar_group("aloop-clean:nightly");

        let rows = aloop_rows(&app);
        assert!(rows
            .iter()
            .any(|row| matches!(row, SidebarRow::AloopCleanRuns { expanded: true, .. })));
        assert!(rows.iter().any(|row| matches!(
            row,
            SidebarRow::AloopCleanRun { loop_name, .. } if loop_name == "nightly"
        )));
    }
}
