//! Prepared fleet inventory with attach-local selection and no I/O while painting.

use std::time::{Duration, SystemTime};

use ratatui::{
    layout::Rect,
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::state::{AppState, DockHostRowHitArea};
use crate::fleet::{EvidenceSource, HostSnapshot, HostState};

const HEADER_ROWS: u16 = 1;

fn host_height(host: &HostSnapshot) -> usize {
    1 + usize::from(host.error.is_some())
        + host
            .entries
            .iter()
            .filter(|entry| entry.source != EvidenceSource::Host)
            .count()
}

fn visible_host_rows(app: &AppState, area: Rect) -> Vec<(&HostSnapshot, u16)> {
    if area.height <= HEADER_ROWS {
        return Vec::new();
    }
    let mut logical_row = 0usize;
    let scroll = usize::from(app.dock_scroll);
    let visible_height = usize::from(area.height - HEADER_ROWS);
    let mut visible = Vec::new();
    for host in &app.fleet_snapshot.hosts {
        let height = host_height(host);
        if logical_row + height > scroll && logical_row < scroll + visible_height {
            let row = logical_row.saturating_sub(scroll);
            if let Ok(row) = u16::try_from(row) {
                visible.push((host, area.y + HEADER_ROWS + row));
            }
        }
        logical_row += height;
    }
    visible
}

pub(crate) fn row_hit_areas(app: &AppState, area: Rect) -> Vec<DockHostRowHitArea> {
    visible_host_rows(app, area)
        .into_iter()
        .filter(|(_, row)| *row < area.bottom())
        .map(|(host, row)| DockHostRowHitArea {
            name: host.name.clone(),
            rect: Rect::new(area.x, row, area.width, 1),
        })
        .collect()
}

pub(crate) fn render_hosts(app: &AppState, frame: &mut Frame, area: Rect) {
    render_hosts_at(app, frame, area, SystemTime::now());
}

fn render_hosts_at(app: &AppState, frame: &mut Frame, area: Rect, now: SystemTime) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    if app.fleet_snapshot.configured_hosts.is_empty() {
        frame.render_widget(
            Paragraph::new(vec![
                Line::from(Span::styled(
                    " No fleet hosts configured",
                    Style::default().fg(app.palette.overlay1),
                )),
                Line::from(Span::styled(
                    " Add [[remote.fleet.hosts]] to config.toml.",
                    Style::default().fg(app.palette.overlay0),
                )),
            ]),
            area,
        );
        return;
    }
    if !app.fleet_snapshot.polled {
        frame.render_widget(
            Paragraph::new(" checking hosts...").style(Style::default().fg(app.palette.overlay1)),
            area,
        );
        return;
    }

    let age = app
        .fleet_snapshot
        .refreshed_at
        .and_then(|then| now.duration_since(then).ok())
        .map(age_label)
        .unwrap_or_else(|| "unknown".to_string());
    frame.render_widget(
        Paragraph::new(format!(
            " hosts {}  refreshed {age}",
            app.fleet_snapshot.hosts.len()
        ))
        .style(Style::default().fg(app.palette.overlay1)),
        Rect::new(area.x, area.y, area.width, 1),
    );

    for (host, row) in visible_host_rows(app, area) {
        render_host(app, frame, area, host, row, now);
    }
}

fn render_host(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    host: &HostSnapshot,
    row: u16,
    now: SystemTime,
) {
    if row >= area.bottom() {
        return;
    }
    let selected = app.dock_hosts_selection.as_deref() == Some(host.name.as_str());
    let row_style = if selected && app.dock_hosts_focused {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.text)
    };
    let (glyph, label, color) = match host.state {
        HostState::Reachable => ("●", "reachable", app.palette.green),
        HostState::Unreachable => ("×", "unreachable", app.palette.red),
        HostState::VersionSkew => ("○", "version skew", app.palette.peach),
    };
    let now_unix_s = now
        .duration_since(SystemTime::UNIX_EPOCH)
        .map_or(0, |duration| duration.as_secs());
    let agent_count = host
        .entries
        .iter()
        .filter(|entry| entry.counts_as_live_agent(host.state, now_unix_s))
        .count();
    let version = host.version.as_deref().unwrap_or("unknown");
    let local = if host.local { "  local" } else { "" };
    let line = Line::from(vec![
        Span::styled(
            format!(" {glyph} "),
            row_style.patch(Style::default().fg(color)),
        ),
        Span::styled(host.name.clone(), row_style),
        Span::styled(
            format!("  {label}  {agent_count} agents  {version}{local}"),
            row_style.patch(Style::default().fg(app.palette.overlay1)),
        ),
    ]);
    frame.render_widget(
        Paragraph::new(line).style(row_style),
        Rect::new(area.x, row, area.width, 1),
    );

    let mut next = row + 1;
    if let Some(error) = host.error.as_deref() {
        if next < area.bottom() {
            frame.render_widget(
                Paragraph::new(format!("   {error}")).style(Style::default().fg(app.palette.red)),
                Rect::new(area.x, next, area.width, 1),
            );
        }
        next += 1;
    }
    for entry in host
        .entries
        .iter()
        .filter(|entry| entry.source != EvidenceSource::Host)
    {
        if next >= area.bottom() {
            break;
        }
        let name = entry.name.as_deref().unwrap_or(entry.handle.as_str());
        let agent = entry.agent.as_deref().unwrap_or("agent");
        let lifecycle = entry.effective_remote_lifecycle(host.state, now_unix_s);
        frame.render_widget(
            Paragraph::new(format!(
                "   {} · {name}  {agent}  {}",
                entry.host, lifecycle.state_label
            ))
            .style(Style::default().fg(app.palette.subtext0)),
            Rect::new(area.x, next, area.width, 1),
        );
        next += 1;
    }
}

fn age_label(age: Duration) -> String {
    if age.as_secs() >= 86_400 {
        format!("{}d", age.as_secs() / 86_400)
    } else if age.as_secs() >= 3_600 {
        format!("{}h", age.as_secs() / 3_600)
    } else if age.as_secs() >= 60 {
        format!("{}m", age.as_secs() / 60)
    } else {
        format!("{}s", age.as_secs())
    }
}

impl AppState {
    pub(crate) fn reconcile_dock_hosts_selection(&mut self) {
        if self
            .fleet_snapshot
            .hosts
            .iter()
            .any(|host| self.dock_hosts_selection.as_deref() == Some(host.name.as_str()))
        {
            return;
        }
        self.dock_hosts_selection = self
            .fleet_snapshot
            .hosts
            .first()
            .map(|host| host.name.clone());
    }

    pub(crate) fn move_dock_hosts_selection(&mut self, delta: isize) {
        if self.fleet_snapshot.hosts.is_empty() {
            self.dock_hosts_selection = None;
            return;
        }
        let current = self
            .dock_hosts_selection
            .as_deref()
            .and_then(|name| {
                self.fleet_snapshot
                    .hosts
                    .iter()
                    .position(|host| host.name == name)
            })
            .unwrap_or(0);
        let next = current
            .saturating_add_signed(delta)
            .min(self.fleet_snapshot.hosts.len() - 1);
        self.dock_hosts_selection = Some(self.fleet_snapshot.hosts[next].name.clone());
    }

    pub(crate) fn selected_fleet_host(&self) -> Option<&HostSnapshot> {
        let selected = self.dock_hosts_selection.as_deref();
        self.fleet_snapshot
            .hosts
            .iter()
            .find(|host| selected == Some(host.name.as_str()))
            .or_else(|| self.fleet_snapshot.hosts.first())
    }

    pub(crate) fn click_dock_host_row(&mut self, col: u16, row: u16) -> Option<String> {
        let hit = self
            .view
            .dock_host_row_hit_areas
            .iter()
            .find(|hit| hit.rect.contains((col, row).into()))?;
        self.dock_hosts_selection = Some(hit.name.clone());
        self.dock_hosts_focused = true;
        Some(hit.name.clone())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn render(app: &AppState, now: SystemTime) -> String {
        let area = Rect::new(0, 0, 72, 12);
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| render_hosts_at(app, frame, area, now))
            .expect("render hosts");
        let buffer = terminal.backend().buffer();
        (0..area.height)
            .map(|row| {
                (0..area.width)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    fn host(name: &str, state: HostState) -> HostSnapshot {
        HostSnapshot {
            name: name.to_string(),
            target: name.to_string(),
            local: false,
            session: None,
            socket: None,
            state,
            version: Some("0.8.2".to_string()),
            protocol: Some(crate::protocol::PROTOCOL_VERSION),
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: false,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }
    }

    #[test]
    fn empty_surface_points_to_the_existing_fleet_config() {
        let app = AppState::test_new();
        let text = render(&app, SystemTime::UNIX_EPOCH);
        assert!(text.contains("No fleet hosts configured"), "{text}");
        assert!(text.contains("remote.fleet.hosts"), "{text}");
    }

    #[test]
    fn surface_shows_skew_error_and_agent_host_identity() {
        let mut app = AppState::test_new();
        let mut skewed = host("ub2", HostState::VersionSkew);
        skewed.version = Some("0.7.9".to_string());
        skewed.entries = vec![crate::fleet::FleetRow::test_agent_row("ub2", "reviewer")];
        let mut unreachable = host("ub1", HostState::Unreachable);
        unreachable.error = Some("ssh: connection refused".to_string());
        app.fleet_snapshot = crate::fleet::Snapshot {
            aloop: None,
            inbox: None,
            polled: true,
            refreshed_at: Some(SystemTime::UNIX_EPOCH),
            refreshed_at_unix_ms: Some(0),
            config_generation: 0,
            configured_hosts: vec!["ub2".to_string(), "ub1".to_string()],
            hosts: vec![skewed, unreachable],
            group_catalogs: Vec::new(),
        };
        let text = render(&app, SystemTime::UNIX_EPOCH + Duration::from_secs(65));
        for expected in [
            "refreshed 1m",
            "version skew",
            "0.7.9",
            "ub2 · reviewer",
            "unreachable",
            "ssh: connection refused",
        ] {
            assert!(text.contains(expected), "missing {expected}: {text}");
        }
    }
}
