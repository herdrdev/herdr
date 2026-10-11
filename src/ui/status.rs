#[cfg(test)]
use std::path::PathBuf;

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::{Block, Borders, Clear, Paragraph},
    Frame,
};

use super::text::{display_width, display_width_u16, truncate_end};
use super::widgets::panel_contrast_fg;
use crate::{
    app::state::{
        CopyFeedback, Palette, StatusButton, StatusButtonAction, StatusSegmentKind, ToastKind,
        ToastNotification, TopBarAction, TopBarView,
    },
    app::AppState,
    config::{StatusIndicatorStyle, ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    terminal::TerminalRuntimeRegistry,
};

#[cfg(test)]
use crate::platform::status_metrics::metric_segment_visible;

/// Full-width top bar.
///
/// Left: one icon per view (Home, Focus, Orchestration, Scratch, Mailbox), the
/// one on screen highlighted. Centre: view name and focused pane title. Right:
/// CPU, memory, disk and `⋯`; host details and actions live in the popover.
///
/// Layout: spans the full client width above the sidebar. The right-hand
/// segments are fitted first; the icons yield to them rather than overlapping,
/// and the view name is drawn only where it fits between the two.
pub(crate) fn render_status_bar(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let p = &app.palette;
    let bg = Style::default().bg(p.panel_bg);
    frame.render_widget(Paragraph::new("").style(bg), area);

    // The pane toggles sit at the far right of this row when the tab row is
    // hidden, so the segments must stop short of them instead of underlapping.
    let reserved = super::tabs::tab_action_status_bar_reserved_width(app, area);
    let content_width = area.width.saturating_sub(reserved);
    if usize::from(content_width) < minimum_required_status_width(app) {
        return;
    }
    // The fitted row normally arrives on the view. A render that never went
    // through view computation, which the unit tests and any future direct
    // draw do, fits it here instead of drawing an empty row.
    let fitted;
    let segments = if app.view.status_segments.is_empty() {
        fitted = fitted_status_segments(app, area);
        &fitted
    } else {
        &app.view.status_segments
    };

    let used = segment_width(segments);
    let pad = (content_width as usize).saturating_sub(used);
    let mut spans: Vec<Span> = Vec::new();
    if pad > 0 {
        spans.push(Span::styled(" ".repeat(pad), bg));
    }
    for seg in segments {
        let style = if seg.preserve_bg {
            seg.style
        } else {
            seg.style.bg(p.panel_bg)
        };
        spans.push(Span::styled(seg.text.clone(), style));
    }
    frame.render_widget(
        Paragraph::new(Line::from(spans)).style(bg),
        Rect::new(area.x, area.y, content_width, area.height),
    );

    let fallback_buttons;
    let buttons = if app.view.status_buttons.is_empty() {
        fallback_buttons = status_buttons(app, area);
        &fallback_buttons
    } else {
        &app.view.status_buttons
    };
    if let Some((rect, text)) = view_title_rect(app, area, buttons, used + usize::from(reserved)) {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                text,
                Style::default().fg(p.overlay1).bg(p.panel_bg),
            ))),
            rect,
        );
    }
    for button in buttons {
        let style = if button.active {
            Style::default().fg(p.accent).bg(p.surface0)
        } else {
            Style::default().fg(p.overlay1).bg(p.panel_bg)
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(button.label.clone(), style))),
            button.rect,
        );
    }
}

#[cfg(test)]
mod fleet_device_tests {
    use super::*;

    fn device(blocked: usize, stale: bool) -> crate::fleet::DeviceAttention {
        crate::fleet::DeviceAttention {
            name: "ub1".into(),
            blocked,
            working: 0,
            stale,
            first_blocked: None,
        }
    }

    #[test]
    fn dots_show_blocker_count_threshold_and_unreachable_state() {
        let palette = Palette::catppuccin();
        assert_eq!(
            fleet_device_text(&device(1, false), &palette).0,
            " \u{25cb}ub1"
        );
        assert_eq!(
            fleet_device_text(&device(2, false), &palette).0,
            " \u{25cb}ub1"
        );
        assert_eq!(
            fleet_device_text(&device(3, false), &palette).0,
            " \u{25cf}ub1"
        );
        assert_eq!(fleet_device_text(&device(0, true), &palette).0, " ?ub1");
    }
}

/// Where the current view's name sits: centred on the bar, or as close to the
/// centre as the icons on its left and the segments on its right allow.
/// `None` when the name does not fit between them at all.
fn view_title_rect(
    app: &AppState,
    area: Rect,
    buttons: &[StatusButton],
    segments_used: usize,
) -> Option<(Rect, String)> {
    let name = app.top_bar_view().name();
    let left = buttons
        .iter()
        .map(|button| button.rect.x.saturating_add(button.rect.width))
        .max()
        .unwrap_or(area.x)
        .saturating_add(1);
    let right = area
        .x
        .saturating_add(area.width)
        .saturating_sub(u16::try_from(segments_used).unwrap_or(u16::MAX))
        .saturating_sub(1);
    let available = usize::from(right.saturating_sub(left));
    if available < display_width(name) {
        return None;
    }
    let parts = focused_pane_title_parts(app);
    let separator = if parts.is_some() { " · " } else { "" };
    let title_budget = available.saturating_sub(display_width(name) + display_width(separator));
    let title =
        parts.and_then(|(repo, thread)| fit_focused_pane_title(&repo, &thread, title_budget));
    let text = match title {
        Some(title) if !title.is_empty() => format!("{name}{separator}{title}"),
        _ => name.to_string(),
    };
    let width = display_width_u16(&text);
    let centred = area.x.saturating_add(area.width.saturating_sub(width) / 2);
    let x = centred.clamp(left, right.saturating_sub(width));
    Some((Rect::new(x, area.y, width, 1), text))
}

/// The right-aligned segments this frame will draw, fitted to the bar.
pub(crate) fn fitted_status_segments(app: &AppState, area: Rect) -> Vec<Segment> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let reserved = super::tabs::tab_action_status_bar_reserved_width(app, area);
    let content_width = area.width.saturating_sub(reserved);
    if usize::from(content_width) < minimum_required_status_width(app) {
        return Vec::new();
    }
    fit_status_segments(
        status_segments(app, metrics_or_unavailable(app), &app.palette),
        usize::from(content_width),
    )
}

fn fit_status_segments(mut segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    if segment_width(&segments) > width {
        for (kind, short) in [
            (StatusSegmentKind::Cpu, "C"),
            (StatusSegmentKind::Memory, "M"),
            (StatusSegmentKind::Disk, "D"),
        ] {
            if let Some(segment) = segments.iter_mut().find(|segment| segment.kind == kind) {
                let value = segment
                    .text
                    .trim()
                    .split_once(' ')
                    .map(|(_, value)| value.trim());
                segment.text = format!(" {short} {} ", value.unwrap_or("--"));
            }
        }
    }
    segments
}

/// What a view icon's tooltip says: its name, what a click does, and the
/// shortcut that does the same, read from the live keybindings.
pub(crate) fn view_button_tooltip(app: &AppState, view: TopBarView) -> String {
    let (what, binding) = match view {
        TopBarView::Home => (
            "All tabs and their agents. Click to return here from any view.",
            &app.keybinds.home_view,
        ),
        TopBarView::Focus => (
            "Hide panes and write in a full-screen notepad. Esc returns Home.",
            &app.keybinds.focus_view,
        ),
        TopBarView::Orchestration => (
            "Board of every agent: working, blocked, done. Opens the board view.",
            &app.keybinds.orchestration_view,
        ),
        TopBarView::Scratch => (
            "Your scratch pad, full screen. Opens the scratch notes writer.",
            &app.keybinds.scratch_view,
        ),
        TopBarView::Mailbox => (
            "Agent requests waiting for you. Opens the blocked inbox.",
            &app.keybinds.mailbox_view,
        ),
    };
    let shortcut = binding
        .labels()
        .into_iter()
        .next()
        .unwrap_or_else(|| "no shortcut".into());
    format!("{}\n{what}\n{shortcut}", view.name())
}

/// The five view icons, left-aligned in a fixed order. Built once per frame so
/// the rendered label and the clickable rect can never disagree.
pub(crate) fn status_buttons(app: &AppState, area: Rect) -> Vec<StatusButton> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let current = app.top_bar_view();
    let content_width = area
        .width
        .saturating_sub(super::tabs::tab_action_status_bar_reserved_width(app, area));
    // The right-aligned segments are load-bearing; icons yield to them rather
    // than overlapping, and drop whole rather than truncating to a stub.
    let reserved = segment_width(&fit_status_segments(
        status_segments(app, metrics_or_unavailable(app), &app.palette),
        usize::from(content_width),
    ));
    let budget = usize::from(content_width).saturating_sub(reserved);
    let mut buttons = Vec::new();
    let mut x = area.x;
    let mut used = 0usize;
    for view in TopBarView::ALL {
        let label = format!(" {} ", crate::ui::icons::view_icon(view, app.nerd_font));
        let width = display_width(&label);
        if used + width > budget {
            break;
        }
        let Ok(cell_width) = u16::try_from(width) else {
            break;
        };
        buttons.push(StatusButton {
            rect: Rect::new(x, area.y, cell_width, 1),
            label,
            action: StatusButtonAction::View(view),
            active: view == current,
        });
        x = x.saturating_add(cell_width);
        used += width;
    }
    buttons
}

fn metrics_or_unavailable(app: &AppState) -> &crate::platform::status_metrics::StatusMetrics {
    static UNAVAILABLE: std::sync::OnceLock<crate::platform::status_metrics::StatusMetrics> =
        std::sync::OnceLock::new();
    app.status_metrics
        .as_ref()
        .map(|snapshot| &snapshot.metrics)
        .unwrap_or_else(|| {
            UNAVAILABLE.get_or_init(|| crate::platform::status_metrics::StatusMetrics {
                hostname: "--".into(),
                ..Default::default()
            })
        })
}

pub(crate) struct Segment {
    text: String,
    style: Style,
    /// When true, `style` already carries its own background (prefix pill).
    preserve_bg: bool,
    kind: StatusSegmentKind,
}

/// Where each fitted segment lands, mirroring the right-aligned layout
/// `render_status_bar` draws from the same stored segments.
pub(crate) fn status_segment_hit_areas(
    app: &AppState,
    area: Rect,
) -> Vec<(StatusSegmentKind, Rect)> {
    let segments = &app.view.status_segments;
    if segments.is_empty() {
        return Vec::new();
    }
    let reserved = super::tabs::tab_action_status_bar_reserved_width(app, area);
    let content_width = usize::from(area.width.saturating_sub(reserved));
    let mut x = usize::from(area.x) + content_width.saturating_sub(segment_width(segments));
    segments
        .iter()
        .map(|segment| {
            let width = display_width(&segment.text);
            let rect = Rect::new(
                u16::try_from(x).unwrap_or(u16::MAX),
                area.y,
                u16::try_from(width).unwrap_or(u16::MAX),
                1,
            );
            x += width;
            (segment.kind, rect)
        })
        .collect()
}

fn segment_width(segments: &[Segment]) -> usize {
    segments
        .iter()
        .map(|segment| display_width(&segment.text))
        .sum()
}

/// The narrowest bar that still keeps all three compact metrics and `⋯`.
pub(crate) fn minimum_required_status_width(_app: &AppState) -> usize {
    display_width(" C 100%  M 999/999G  D 100%  ... ")
}

const WARN_PERCENT: u8 = 80;
const CRITICAL_PERCENT: u8 = 90;
const DOT: char = '\u{25cf}';
const FILL_LEVELS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];

pub(crate) fn fill_glyph(percent: u8) -> char {
    let index = usize::from(percent.min(100)) * FILL_LEVELS.len() / 101;
    FILL_LEVELS[index.min(FILL_LEVELS.len() - 1)]
}

pub(crate) fn load_color(percent: u8, p: &Palette) -> Color {
    match percent {
        percent if percent >= CRITICAL_PERCENT => p.red,
        percent if percent >= WARN_PERCENT => p.yellow,
        _ => p.green,
    }
}

pub(crate) fn fleet_device_text(
    device: &crate::fleet::DeviceAttention,
    p: &Palette,
) -> (String, Style) {
    if device.stale {
        return (
            format!(" ?{}", device.name),
            Style::default().fg(p.overlay0).add_modifier(Modifier::DIM),
        );
    }
    let dot = if device.blocked >= 3 { DOT } else { '\u{25cb}' };
    (format!(" {dot}{}", device.name), Style::default().fg(p.red))
}

/// The bar's right-hand segments: the threshold metrics, then `⋯`.
fn status_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();
    let cpu = metrics.cpu_percent.filter(|value| *value <= 100);
    let memory = memory_percent(metrics);
    let disk = metrics.disk_percent.filter(|value| *value <= 100);
    let memory_value = match (metrics.mem_used_gib, metrics.mem_total_gib) {
        (Some(used), Some(total)) if used.is_finite() && total.is_finite() && total > 0.0 => {
            Some(format!("{used:.1}/{total:.0}G"))
        }
        _ => None,
    };
    for (kind, label, value, percent, was_visible) in [
        (
            StatusSegmentKind::Cpu,
            "CPU",
            cpu.map(|n| format!("{n}%")),
            cpu,
            app.status_cpu_visible,
        ),
        (
            StatusSegmentKind::Memory,
            "MEM",
            memory_value,
            memory,
            app.status_mem_visible,
        ),
        (
            StatusSegmentKind::Disk,
            "DSK",
            disk.map(|n| format!("{n}%")),
            disk,
            app.status_disk_visible,
        ),
    ] {
        out.push(compact_metric_segment(
            kind,
            label,
            value.as_deref(),
            percent,
            was_visible,
            p,
        ));
    }
    out.push(Segment {
        text: format!(" {} ", crate::ui::icons::more_label(app.nerd_font)),
        style: Style::default().fg(if app.status_more_open {
            p.accent
        } else {
            p.overlay1
        }),
        preserve_bg: false,
        kind: StatusSegmentKind::More,
    });
    out
}

/// One clickable piece of a "more" popover row.
pub(crate) struct MoreItem {
    pub kind: StatusSegmentKind,
    pub text: String,
    pub style: Style,
}

/// The open "more" popover: its bordered rect, its rows, and the hit area of
/// every item, registered beside the bar's own segments so clicks and hover
/// tooltips reuse the status-segment paths.
pub(crate) struct StatusMoreLayout {
    pub rect: Rect,
    pub rows: Vec<Vec<MoreItem>>,
    pub hit_areas: Vec<(StatusSegmentKind, Rect)>,
}

/// The indicators the bar hides by default, one row each: the metric-number
/// toggle, link, agents, remote and local host, then the fleet.
fn status_more_rows(app: &AppState, p: &Palette) -> Vec<Vec<MoreItem>> {
    let item = |kind, text: String, style| MoreItem { kind, text, style };
    let mut rows = Vec::new();

    let online = app.connectivity.is_online();
    rows.push(vec![item(
        StatusSegmentKind::Link,
        format!("{DOT} Link {}", if online { "online" } else { "offline" }),
        Style::default().fg(if online { p.green } else { p.red }),
    )]);

    let (agents, blocked) = app.agent_dot_counts();
    if agents > 0 {
        rows.push(vec![item(
            StatusSegmentKind::Agents,
            format!("{DOT} {agents} agents · {blocked} waiting"),
            Style::default().fg(if blocked > 0 { p.red } else { p.accent }),
        )]);
    }

    if let Some(host) = app.view.focused_remote_host.as_deref() {
        rows.push(vec![item(
            StatusSegmentKind::RemoteHost,
            format!("\u{2192} {host}"),
            Style::default().fg(p.teal),
        )]);
    }
    rows.push(vec![item(
        StatusSegmentKind::Hostname,
        format!("Host {}", app.chrome_host_label.trim()),
        Style::default().fg(p.green),
    )]);

    // Fleet availability is a compact, explorable host list. The local config
    // is the inventory, so configured machines remain visible without a poll.
    let has_configured_hosts = app.machines.iter().any(|machine| !machine.is_local());
    let has_remote_snapshot = app.fleet_snapshot.hosts.iter().any(|host| !host.local);
    let attention_devices = if has_remote_snapshot {
        app.fleet_snapshot.devices_needing_attention()
    } else {
        Vec::new()
    };
    if has_configured_hosts || !attention_devices.is_empty() {
        rows.push(vec![item(
            StatusSegmentKind::FleetLabel,
            format!(
                "{} Fleet {}",
                if app.nerd_font { "\u{F013B}" } else { "⧉" },
                if app.fleet_status { "shown" } else { "hidden" }
            ),
            Style::default().fg(p.overlay1),
        )]);
        if app.fleet_status {
            for (idx, device) in attention_devices.iter().enumerate() {
                let (text, style) = fleet_device_text(device, p);
                let text = if device.stale {
                    text.trim_start().to_string()
                } else {
                    format!("{} · {} blockers", text.trim_start(), device.blocked)
                };
                rows.push(vec![item(StatusSegmentKind::FleetDevice(idx), text, style)]);
            }
            for (idx, host) in app
                .machines
                .iter()
                .filter(|machine| !machine.is_local())
                .enumerate()
            {
                let state = app
                    .fleet_snapshot
                    .hosts
                    .iter()
                    .find(|snapshot| snapshot.name == host.name)
                    .map(|snapshot| snapshot.state);
                let (dot, color) = match state {
                    Some(crate::fleet::HostState::Reachable) => (DOT, p.green),
                    Some(crate::fleet::HostState::Unreachable) => (DOT, p.red),
                    Some(crate::fleet::HostState::VersionSkew) => (DOT, p.peach),
                    None => ('·', p.overlay0),
                };
                rows.push(vec![
                    item(
                        StatusSegmentKind::FleetHost(idx),
                        format!("{dot}{}  ", host.name),
                        Style::default().fg(color),
                    ),
                    item(
                        StatusSegmentKind::FleetUseMachine(idx),
                        (if app.nerd_font { "\u{F0A9}" } else { ">" }).to_string(),
                        Style::default().fg(p.overlay1),
                    ),
                ]);
            }
        }
    }

    let on_status_bar = |rect: Rect| {
        app.mouse_capture
            && rect.width > 0
            && rect.y == app.view.status_bar_rect.y
            && app.view.status_bar_rect.height > 0
    };
    let mut panes = Vec::new();
    for (rect, direction, label) in [
        (
            app.view.pane_toggle_below_hit_area,
            crate::app::state::PaneToggleDirection::Below,
            "Split below",
        ),
        (
            app.view.pane_toggle_right_hit_area,
            crate::app::state::PaneToggleDirection::Right,
            "Split right",
        ),
    ] {
        if on_status_bar(rect) {
            panes.push(item(
                StatusSegmentKind::MoreAction(TopBarAction::PaneToggle(direction)),
                label.into(),
                Style::default().fg(p.overlay1),
            ));
        }
    }
    if !panes.is_empty() {
        rows.push(vec![item(
            StatusSegmentKind::MoreGroup,
            "Panes".into(),
            Style::default().fg(p.accent),
        )]);
        rows.extend(panes.into_iter().map(|item| vec![item]));
    }

    let mut actions = Vec::new();
    if on_status_bar(app.view.repo_editor_button_hit_area) {
        actions.push(item(
            StatusSegmentKind::MoreAction(TopBarAction::RepoEditor),
            "Open editor".into(),
            Style::default().fg(p.overlay1),
        ));
    }
    if on_status_bar(app.view.add_action_button_hit_area) {
        actions.push(item(
            StatusSegmentKind::MoreAction(TopBarAction::AddAction),
            "New action".into(),
            Style::default().fg(p.overlay1),
        ));
    }
    for (index, rect) in &app.view.user_action_hit_areas {
        if on_status_bar(*rect) {
            if let Some(action) = app.keybinds.user_actions.get(*index) {
                actions.push(item(
                    StatusSegmentKind::MoreAction(TopBarAction::UserAction(*index)),
                    action.name.clone(),
                    Style::default().fg(p.overlay1),
                ));
            }
        }
    }
    if on_status_bar(app.view.git_menu_button_hit_area) {
        actions.push(item(
            StatusSegmentKind::MoreAction(TopBarAction::GitMenu),
            "Git actions".into(),
            Style::default().fg(p.overlay1),
        ));
    }
    if !actions.is_empty() {
        rows.push(vec![item(
            StatusSegmentKind::MoreGroup,
            "Actions".into(),
            Style::default().fg(p.accent),
        )]);
        rows.extend(actions.into_iter().map(|item| vec![item]));
    }

    rows.push(vec![item(
        StatusSegmentKind::StatusDetail,
        format!(
            "{} Metric numbers {}",
            if app.status_bar_expanded {
                "▾"
            } else {
                "▸"
            },
            if app.status_bar_expanded { "on" } else { "off" }
        ),
        Style::default().fg(p.overlay1),
    )]);
    rows
}

/// Geometry of the open "more" popover, anchored under the `⋯` control at the
/// bar's right edge. `None` while it is closed or the screen has no room.
pub(crate) fn status_more_layout(
    app: &AppState,
    bar: Rect,
    screen: Rect,
) -> Option<StatusMoreLayout> {
    if !app.status_more_open || bar.width == 0 || bar.height == 0 {
        return None;
    }
    let rows = status_more_rows(app, &app.palette);
    let inner_width = rows
        .iter()
        .map(|row| {
            row.iter()
                .map(|item| display_width(&item.text))
                .sum::<usize>()
        })
        .max()
        .unwrap_or(0)
        .saturating_add(2);
    let available_height = screen.bottom().saturating_sub(bar.y.saturating_add(1));
    let width = u16::try_from(inner_width.saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(screen.width);
    let height = u16::try_from(rows.len().saturating_add(2))
        .unwrap_or(u16::MAX)
        .min(available_height);
    if width < 4 || height < 3 {
        return None;
    }
    let right = bar.x.saturating_add(
        bar.width
            .saturating_sub(super::tabs::tab_action_status_bar_reserved_width(app, bar)),
    );
    let rect = Rect::new(right.saturating_sub(width), bar.y + 1, width, height);
    let inner_right = rect.right().saturating_sub(1);
    let mut hit_areas = Vec::new();
    for (row_idx, row) in rows.iter().enumerate().take(usize::from(height - 2)) {
        let y = rect.y + 1 + u16::try_from(row_idx).unwrap_or(u16::MAX);
        let mut x = rect.x + 2;
        for item in row {
            let item_width = display_width_u16(&item.text).min(inner_right.saturating_sub(x));
            if item_width == 0 {
                break;
            }
            hit_areas.push((item.kind, Rect::new(x, y, item_width, 1)));
            x = x.saturating_add(item_width);
        }
    }
    Some(StatusMoreLayout {
        rect,
        rows,
        hit_areas,
    })
}

/// Draw the open "more" popover over whatever sits below the bar.
pub(crate) fn render_status_more_menu(app: &AppState, frame: &mut Frame) {
    let Some(layout) = status_more_layout(app, app.view.status_bar_rect, frame.area()) else {
        return;
    };
    let p = &app.palette;
    frame.render_widget(Clear, layout.rect);
    let lines = layout
        .rows
        .iter()
        .map(|row| {
            let mut spans = vec![Span::raw(" ")];
            spans.extend(
                row.iter()
                    .map(|item| Span::styled(item.text.clone(), item.style.bg(p.panel_bg))),
            );
            Line::from(spans)
        })
        .collect::<Vec<_>>();
    frame.render_widget(
        Paragraph::new(lines).block(
            Block::default()
                .borders(Borders::ALL)
                .border_style(Style::default().fg(p.overlay0))
                .style(Style::default().bg(p.panel_bg)),
        ),
        layout.rect,
    );
}

/// Memory as a share of the installed total, the form that needs no arithmetic
/// from the reader.
pub(crate) fn memory_percent(
    metrics: &crate::platform::status_metrics::StatusMetrics,
) -> Option<u8> {
    let used = metrics.mem_used_gib?;
    let total = metrics.mem_total_gib?;
    if !used.is_finite() || !total.is_finite() || total <= 0.0 || used < 0.0 || used > total {
        return None;
    }
    Some((used / total * 100.0).round().clamp(0.0, 100.0) as u8)
}

/// Resolve the focused pane's remote host once for the current view. Proxy
/// panes use their cached configured host; attached panes use the same
/// launch-argv lookup as the sidebar.
pub(crate) fn resolve_focused_remote_host(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<String> {
    let workspace = app.workspaces.get(app.active?)?;
    let terminal_id = workspace.terminal_id(workspace.focused_pane_id()?)?;
    let terminal = app.terminals.get(terminal_id)?;
    if terminal_runtimes
        .get(terminal_id)
        .is_some_and(crate::terminal::TerminalRuntime::is_remote_proxy)
    {
        return terminal.remote_proxy_host.clone();
    }
    terminal
        .launch_argv
        .as_deref()
        .and_then(|argv| crate::fleet::attached_host_name(&app.fleet_snapshot, argv))
        .map(str::to_owned)
}

fn compact_metric_segment(
    kind: StatusSegmentKind,
    label: &str,
    value: Option<&str>,
    percent: Option<u8>,
    was_visible: bool,
    p: &Palette,
) -> Segment {
    let healthy =
        percent.is_some_and(|value| value < WARN_PERCENT && !(was_visible && value >= 78));
    Segment {
        text: if healthy {
            format!(" {DOT} ")
        } else {
            format!(" {label} {} ", value.unwrap_or("--"))
        },
        style: Style::default().fg(match percent {
            Some(value) if value >= CRITICAL_PERCENT => p.red,
            Some(value) if value >= WARN_PERCENT || (was_visible && value >= 78) => p.yellow,
            _ => p.green,
        }),
        preserve_bg: false,
        kind,
    }
}

/// The focused pane's repository and session title share the same source as
/// the sidebar, so the bar cannot name a pane differently from its row.
fn focused_pane_title_parts(app: &AppState) -> Option<(String, String)> {
    let workspace = app.active.and_then(|ws_idx| app.workspaces.get(ws_idx))?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal = app.terminals.get(workspace.terminal_id(pane_id)?)?;
    let context = terminal.effective_work_context();
    let repo = context
        .repo
        .as_deref()
        .and_then(|repo| repo.rsplit('/').find(|part| !part.trim().is_empty()))
        .map(str::trim)
        .filter(|repo| !repo.is_empty())
        .map(str::to_string)
        .or_else(|| {
            terminal
                .cwd
                .file_name()
                .and_then(|name| name.to_str())
                .map(str::trim)
                .filter(|repo| !repo.is_empty())
                .map(str::to_string)
        })?;
    let tab_idx = workspace.active_tab;
    let projection = workspace.tab_display_projection(&app.terminals, tab_idx);
    let title = crate::workspace::session_title(
        projection.as_ref(),
        workspace
            .tab_display_name_from(&app.terminals, tab_idx)
            .or_else(|| Some(super::sidebar::DEFAULT_THREAD_TITLE.to_string())),
    )?;
    let title = title.trim();
    (!title.is_empty()).then(|| (repo, title.to_string()))
}

fn fit_focused_pane_title(repo: &str, thread: &str, width: usize) -> Option<String> {
    let repo_width = display_width(repo);
    if repo_width == 0 || repo_width > width {
        return None;
    }
    let full = format!("{repo} / {thread}");
    if display_width(&full) <= width {
        return Some(full);
    }
    let minimum = format!("{repo} / …");
    if display_width(&minimum) > width {
        return Some(repo.to_string());
    }
    Some(truncate_end(&full, width))
}

#[cfg(test)]
/// Focused pane cwd and branch, still projected for consumers that need the
/// Git context. The status row no longer renders the branch: it never changed
/// within a workspace, so it cost a column without answering a question.
pub(crate) fn focused_context(app: &AppState) -> (Option<PathBuf>, Option<String>) {
    let Some(ws_idx) = app.active else {
        return (None, None);
    };
    let Some(ws) = app.workspaces.get(ws_idx) else {
        return (None, None);
    };
    // Runtime-resolved focused cwd, projected by the same code path that
    // resolves the Git refresh target, so the cwd and branch segments always
    // describe the same directory. Falls back to workspace identity until the
    // first projection lands.
    let cwd = app
        .status_focused_cwd
        .clone()
        .or_else(|| (!app.status_focus_projection_initialized).then(|| ws.identity_cwd.clone()));
    let branch = if app.status_git_cwd.as_ref() == cwd.as_ref() {
        app.status_git_branch.clone()
    } else if !app.status_focus_projection_initialized
        && ws.cached_identity_cwd == cwd.clone().unwrap_or_default()
    {
        ws.cached_git_branch.clone()
    } else {
        None
    };
    (cwd, branch)
}

pub(crate) fn copy_feedback_rect(
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
) -> Rect {
    if area.width == 0 || area.height == 0 {
        return Rect::default();
    }

    let content_width = feedback.message.len() as u16 + 4;
    let width = content_width.min(area.width);
    let height = 3u16.min(area.height);
    let x = match position {
        ToastClipboardPosition::TopLeft | ToastClipboardPosition::BottomLeft => area.x,
        ToastClipboardPosition::TopCenter | ToastClipboardPosition::BottomCenter => {
            area.x + area.width.saturating_sub(width) / 2
        }
        ToastClipboardPosition::TopRight | ToastClipboardPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let y = match position {
        ToastClipboardPosition::TopLeft
        | ToastClipboardPosition::TopCenter
        | ToastClipboardPosition::TopRight => area.y + offset_rows.min(area.height),
        ToastClipboardPosition::BottomLeft
        | ToastClipboardPosition::BottomCenter
        | ToastClipboardPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + offset_rows)
        }
    };
    Rect::new(x, y, width, height)
}

pub(crate) fn toast_notification_rect(
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
) -> Rect {
    let content_width = display_width_u16(&toast.title)
        .max(display_width_u16(&toast.context))
        .saturating_add(4);
    let width = content_width.saturating_add(2).min(area.width);
    let content_height = if toast.context.is_empty() { 1 } else { 2 };
    let height = (content_height + 2).min(area.height);
    let x = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::BottomLeft => area.x,
        ToastHerdrPosition::TopRight | ToastHerdrPosition::BottomRight => {
            area.x + area.width.saturating_sub(width)
        }
    };
    let warning_offset = u16::from(offset_for_warning);
    let y = match position {
        ToastHerdrPosition::TopLeft | ToastHerdrPosition::TopRight => {
            area.y + warning_offset.min(area.height)
        }
        ToastHerdrPosition::BottomLeft | ToastHerdrPosition::BottomRight => {
            area.y + area.height.saturating_sub(height + warning_offset)
        }
    };
    Rect::new(x, y, width, height)
}

pub(super) fn render_toast_notification(
    frame: &mut Frame,
    area: Rect,
    toast: &ToastNotification,
    offset_for_warning: bool,
    position: ToastHerdrPosition,
    p: &Palette,
) {
    let dot_color = match toast.kind {
        ToastKind::NeedsAttention => p.red,
        ToastKind::Finished => p.blue,
        ToastKind::UpdateInstalled | ToastKind::WorkLinked => p.accent,
    };
    let toast_area = toast_notification_rect(area, toast, offset_for_warning, position);

    frame.render_widget(Clear, toast_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.overlay0))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(toast_area);
    frame.render_widget(block, toast_area);

    if inner.height < 1 {
        return;
    }

    let [title_row, context_row] =
        Layout::vertical([Constraint::Length(1), Constraint::Length(1)]).areas(inner);

    let title = Line::from(vec![
        Span::styled("●", Style::default().fg(dot_color)),
        Span::raw(" "),
        Span::styled(
            &toast.title,
            Style::default().fg(p.text).add_modifier(Modifier::BOLD),
        ),
    ]);
    let context = Line::from(vec![
        Span::styled("  ", Style::default().fg(p.overlay0)),
        Span::styled(&toast.context, Style::default().fg(p.overlay0)),
    ]);

    frame.render_widget(Paragraph::new(title), title_row);
    if !toast.context.is_empty() && inner.height >= 2 {
        frame.render_widget(Paragraph::new(context), context_row);
    }
}

pub(super) fn render_copy_feedback(
    frame: &mut Frame,
    area: Rect,
    feedback: &CopyFeedback,
    offset_rows: u16,
    position: ToastClipboardPosition,
    p: &Palette,
) {
    let feedback_area = copy_feedback_rect(area, feedback, offset_rows, position);
    if feedback_area.is_empty() {
        return;
    }

    frame.render_widget(Clear, feedback_area);
    let block = Block::default()
        .borders(Borders::ALL)
        .border_style(Style::default().fg(p.green))
        .style(Style::default().bg(p.panel_bg));
    let inner = block.inner(feedback_area);
    frame.render_widget(block, feedback_area);

    if inner.height == 0 {
        return;
    }

    let text = Line::from(vec![
        Span::styled("●", Style::default().fg(p.green).bg(p.panel_bg)),
        Span::raw(" "),
        Span::styled(
            &feedback.message,
            Style::default()
                .fg(p.text)
                .bg(p.panel_bg)
                .add_modifier(Modifier::BOLD),
        ),
    ]);
    frame.render_widget(Paragraph::new(text), inner);
}

pub(super) fn config_diagnostic_marker_rect(area: Rect) -> Rect {
    let width = area.width.min(3);
    Rect::new(
        area.right().saturating_sub(width),
        area.y,
        width,
        u16::from(area.height > 0),
    )
}

pub(super) fn render_config_diagnostic(
    frame: &mut Frame,
    area: Rect,
    message: &str,
    expanded: bool,
    p: &Palette,
) {
    let style = Style::default()
        .fg(panel_contrast_fg(p))
        .bg(p.yellow)
        .add_modifier(Modifier::BOLD);

    if !expanded {
        let marker_area = config_diagnostic_marker_rect(area);
        if marker_area.is_empty() {
            return;
        }
        let marker = match marker_area.width {
            1 => "!",
            2 => "! ",
            _ => " ! ",
        };
        frame.render_widget(Clear, marker_area);
        frame.render_widget(Paragraph::new(Span::styled(marker, style)), marker_area);
        return;
    }

    for (row, line) in message
        .lines()
        .filter(|line| !line.trim().is_empty())
        .take(area.height as usize)
        .enumerate()
    {
        let text = format!(" {line} ");
        let width = (text.len() as u16).min(area.width);
        let notif_area = Rect::new(
            area.x + area.width.saturating_sub(width),
            area.y + row as u16,
            width,
            1,
        );

        frame.render_widget(Clear, notif_area);
        frame.render_widget(Paragraph::new(Span::styled(text, style)), notif_area);
    }
}

pub(super) fn state_icon_symbol(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
) -> &'static str {
    match (indicator_style, state, seen) {
        (StatusIndicatorStyle::Dots, AgentState::Blocked, _)
        | (StatusIndicatorStyle::Dots, AgentState::Working, _)
        | (StatusIndicatorStyle::Dots, AgentState::Idle, false) => "●",
        (StatusIndicatorStyle::Dots, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Dots, AgentState::Unknown, _) => "·",
        (StatusIndicatorStyle::Symbols, AgentState::Blocked, _) => "×",
        (StatusIndicatorStyle::Symbols, AgentState::Working, _) => "◐",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, false) => "✓",
        (StatusIndicatorStyle::Symbols, AgentState::Idle, true) => "○",
        (StatusIndicatorStyle::Symbols, AgentState::Unknown, _) => "·",
    }
}

pub(super) fn state_icon(
    state: AgentState,
    seen: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    (
        state_icon_symbol(state, seen, indicator_style),
        Style::default().fg(state_label_color(state, seen, p)),
    )
}

pub(super) fn state_icon_with_stale(
    state: AgentState,
    seen: bool,
    stale: bool,
    indicator_style: StatusIndicatorStyle,
    p: &Palette,
) -> (&'static str, Style) {
    let _ = stale;
    state_icon(state, seen, indicator_style, p)
}

#[cfg(test)]
pub(super) fn state_label(state: AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (AgentState::Blocked, _) => "blocked",
        (AgentState::Working, _) => "working",
        (AgentState::Idle, false) => "done",
        (AgentState::Idle, true) => "idle",
        (AgentState::Unknown, _) => "idle",
    }
}

pub(super) fn state_label_color(state: AgentState, seen: bool, p: &Palette) -> Color {
    match (state, seen) {
        (AgentState::Blocked, _) => p.red,
        (AgentState::Working, _) => p.blue,
        (AgentState::Idle, false) => p.blue,
        (AgentState::Idle, true) => p.green,
        (AgentState::Unknown, _) => p.overlay0,
    }
}

pub(super) fn state_label_color_with_stale(
    state: AgentState,
    seen: bool,
    stale: bool,
    p: &Palette,
) -> Color {
    let _ = stale;
    state_label_color(state, seen, p)
}

#[cfg(test)]
pub(super) fn status_report_age_label(
    reported_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<String> {
    let age = now.checked_duration_since(reported_at?)?;
    (age >= std::time::Duration::from_secs(60)).then(|| {
        let minutes = age.as_secs() / 60;
        if minutes >= 60 {
            format!("reported {}h ago", minutes / 60)
        } else {
            format!("reported {minutes}m ago")
        }
    })
}

/// Three-column sidebar age: blank under one minute, then `3m`, `12m`, `1h`,
/// `2d`. Days cap at `99d` so the label never outgrows its field.
pub(super) fn status_report_age_micro_label(
    reported_at: std::time::Instant,
    now: std::time::Instant,
) -> String {
    let seconds = now.saturating_duration_since(reported_at).as_secs();
    if seconds < 60 {
        String::new()
    } else if seconds < 3_600 {
        format!("{}m", seconds / 60)
    } else if seconds < 86_400 {
        format!("{}h", seconds / 3_600)
    } else {
        format!("{}d", (seconds / 86_400).min(99))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastHerdrPosition};
    use crate::platform::status_metrics::StatusMetrics;

    fn toast() -> ToastNotification {
        ToastNotification {
            kind: ToastKind::Finished,
            title: "done".to_string(),
            context: "workspace".to_string(),
            position: None,
            target: None,
        }
    }

    fn feedback() -> CopyFeedback {
        CopyFeedback {
            message: "copied to clipboard".to_string(),
        }
    }

    #[test]
    fn state_dots_and_labels_use_semantic_workspace_colors() {
        let palette = Palette::catppuccin();
        for (state, seen, symbol, color) in [
            (AgentState::Blocked, true, "●", palette.red),
            // ac7: active work uses the blue activity accent, not warning yellow.
            (AgentState::Working, true, "●", palette.blue),
            (AgentState::Idle, false, "●", palette.blue),
            (AgentState::Idle, true, "○", palette.green),
            (AgentState::Unknown, true, "·", palette.overlay0),
        ] {
            let (actual_symbol, style) =
                state_icon(state, seen, StatusIndicatorStyle::Dots, &palette);
            assert_eq!(actual_symbol, symbol);
            assert_eq!(style.fg, Some(color));
            assert_eq!(state_label_color(state, seen, &palette), color);
        }
    }

    #[test]
    fn state_icons_support_dot_and_distinct_symbol_styles() {
        let palette = Palette::catppuccin();
        for (indicator_style, expected_symbols) in [
            (StatusIndicatorStyle::Dots, ["●", "●", "●", "○", "·"]),
            (StatusIndicatorStyle::Symbols, ["×", "◐", "✓", "○", "·"]),
        ] {
            for ((state, seen, color), expected_symbol) in [
                (AgentState::Blocked, true, palette.red),
                // The fork's sidebar contract uses blue for active work.
                (AgentState::Working, true, palette.blue),
                (AgentState::Idle, false, palette.blue),
                (AgentState::Idle, true, palette.green),
                (AgentState::Unknown, true, palette.overlay0),
            ]
            .into_iter()
            .zip(expected_symbols)
            {
                let (actual_symbol, style) = state_icon(state, seen, indicator_style, &palette);
                assert_eq!(actual_symbol, expected_symbol);
                assert_eq!(display_width_u16(actual_symbol), 1);
                assert_eq!(style.fg, Some(color));
            }
        }
    }

    #[test]
    fn status_report_age_projection_distinguishes_recent_and_aged_reports() {
        let reported_at = std::time::Instant::now();
        let recent = reported_at + std::time::Duration::from_secs(30);
        let aged = reported_at + std::time::Duration::from_secs(90);
        let old = reported_at + std::time::Duration::from_secs(3660);

        assert_eq!(status_report_age_label(Some(reported_at), recent), None);
        assert_eq!(
            status_report_age_label(Some(reported_at), aged).as_deref(),
            Some("reported 1m ago")
        );
        assert_eq!(
            status_report_age_micro_label(reported_at, old).as_str(),
            "1h"
        );
        let (stale_symbol, stale_style) = state_icon_with_stale(
            AgentState::Working,
            true,
            true,
            StatusIndicatorStyle::Dots,
            &Palette::catppuccin(),
        );
        assert_eq!(stale_symbol, "●");
        assert_eq!(stale_style.fg, Some(Palette::catppuccin().blue));
    }

    #[test]
    fn toast_rect_uses_configured_corner() {
        let area = Rect::new(10, 20, 100, 40);
        let toast = toast();

        let top_left = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopLeft);
        assert_eq!(top_left.x, area.x);
        assert_eq!(top_left.y, area.y);

        let top_right = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);
        assert_eq!(top_right.x + top_right.width, area.x + area.width);
        assert_eq!(top_right.y, area.y);

        let bottom_left =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomLeft);
        assert_eq!(bottom_left.x, area.x);
        assert_eq!(bottom_left.y + bottom_left.height, area.y + area.height);

        let bottom_right =
            toast_notification_rect(area, &toast, false, ToastHerdrPosition::BottomRight);
        assert_eq!(bottom_right.x + bottom_right.width, area.x + area.width);
        assert_eq!(bottom_right.y + bottom_right.height, area.y + area.height);
    }

    #[test]
    fn toast_rect_uses_display_width_for_cjk_labels() {
        let area = Rect::new(0, 0, 100, 20);
        let toast = ToastNotification {
            kind: ToastKind::NeedsAttention,
            title: "重构用户认证模块".to_string(),
            context: "提交 herdr 的反馈".to_string(),
            position: None,
            target: None,
        };

        let rect = toast_notification_rect(area, &toast, false, ToastHerdrPosition::TopRight);

        let expected_content_width =
            display_width_u16(&toast.title).max(display_width_u16(&toast.context)) + 6;
        assert_eq!(rect.width, expected_content_width);
        assert_eq!(rect.x + rect.width, area.x + area.width);
    }

    #[test]
    fn copy_feedback_rect_uses_configured_position() {
        let area = Rect::new(10, 20, 100, 40);
        let feedback = feedback();

        let top_center = copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::TopCenter);
        assert_eq!(top_center.y, area.y);
        assert_eq!(
            top_center.x,
            area.x + area.width.saturating_sub(top_center.width) / 2
        );

        let bottom_center =
            copy_feedback_rect(area, &feedback, 0, ToastClipboardPosition::BottomCenter);
        assert_eq!(bottom_center.y + bottom_center.height, area.y + area.height);
        assert_eq!(
            bottom_center.x,
            area.x + area.width.saturating_sub(bottom_center.width) / 2
        );
    }

    #[test]
    fn status_branch_is_bound_to_focused_pane_cwd() {
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/repo");
        workspace.cached_identity_cwd = PathBuf::from("/repo");
        workspace.cached_git_branch = Some("workspace-root".into());
        let focused = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let focused_terminal = workspace
            .terminal_id(focused)
            .expect("focused terminal")
            .clone();
        let mut app = AppState::test_new();
        app.terminals.insert(
            focused_terminal.clone(),
            crate::terminal::TerminalState::new(focused_terminal, PathBuf::from("/repo/nested")),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.sync_status_focused_cwd(&crate::terminal::TerminalRuntimeRegistry::default());
        app.status_git_cwd = Some(PathBuf::from("/repo/nested"));
        app.status_git_branch = Some("nested-branch".into());

        let (cwd, branch) = focused_context(&app);

        assert_eq!(cwd, Some(PathBuf::from("/repo/nested")));
        assert_eq!(branch.as_deref(), Some("nested-branch"));
    }

    #[test]
    fn scheduled_focus_projection_hides_stale_branch() {
        let root_cwd = PathBuf::from("/repo");
        let nested_cwd = root_cwd.join("nested");
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = root_cwd.clone();
        workspace.cached_identity_cwd = root_cwd.clone();
        workspace.cached_git_branch = Some("root-branch".into());
        let root = workspace.tabs[0].root_pane;
        let root_terminal = workspace.terminal_id(root).expect("root terminal").clone();
        let nested = workspace.test_split(ratatui::layout::Direction::Horizontal);
        let nested_terminal = workspace
            .terminal_id(nested)
            .expect("nested terminal")
            .clone();
        workspace.tabs[0].layout.focus_pane(root);

        let mut app = AppState::test_new();
        app.terminals.insert(
            root_terminal.clone(),
            crate::terminal::TerminalState::new(root_terminal, root_cwd.clone()),
        );
        app.terminals.insert(
            nested_terminal.clone(),
            crate::terminal::TerminalState::new(nested_terminal, nested_cwd.clone()),
        );
        app.workspaces = vec![workspace];
        app.active = Some(0);
        let runtimes = crate::terminal::TerminalRuntimeRegistry::default();

        assert!(app.sync_status_focused_cwd(&runtimes));
        app.status_git_cwd = Some(root_cwd.clone());
        app.status_git_branch = Some("root-branch".into());
        assert_eq!(app.status_focused_cwd, Some(root_cwd));

        assert!(app.focus_pane_in_workspace(0, nested));
        assert!(app.sync_status_focused_cwd(&runtimes));
        let (cwd, branch) = focused_context(&app);

        assert_eq!(app.status_focused_cwd, Some(nested_cwd.clone()));
        assert_eq!(cwd, Some(nested_cwd));
        assert_eq!(branch, None);
    }

    #[test]
    fn status_cwd_falls_back_to_workspace_identity_before_first_projection() {
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/repo");
        workspace.cached_identity_cwd = PathBuf::from("/repo");
        workspace.cached_git_branch = Some("workspace-root".into());
        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);

        let (cwd, branch) = focused_context(&app);

        assert_eq!(cwd, Some(PathBuf::from("/repo")));
        assert_eq!(branch.as_deref(), Some("workspace-root"));
    }

    fn fleet_host_fixture() -> crate::fleet::HostSnapshot {
        crate::fleet::HostSnapshot {
            name: "workbox".to_string(),
            target: "you@workbox".to_string(),
            local: false,
            session: Some("agents".to_string()),
            socket: None,
            state: crate::fleet::HostState::Reachable,
            version: None,
            protocol: None,
            error: None,
            remote_identity: None,
            sessions: None,
            reachable: true,
            last_seen_unix_ms: None,
            entries: Vec::new(),
        }
    }

    fn app_with_focused_attached_pane() -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let host = fleet_host_fixture();
        let argv = crate::fleet::agent_attach_argv(&host, "w1:p2").expect("attach argv");
        let terminal_id = app.workspaces[0]
            .terminal_id(app.workspaces[0].focused_pane_id().expect("focused pane"))
            .expect("focused terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .launch_argv = Some(argv);
        app.fleet_snapshot = crate::fleet::Snapshot {
            hosts: vec![host],
            ..crate::fleet::Snapshot::default()
        };
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: crate::platform::status_metrics::status_metrics_fixture(),
            sampled_at: std::time::Instant::now(),
        });
        app.view.focused_remote_host =
            resolve_focused_remote_host(&app, &TerminalRuntimeRegistry::new());
        app
    }

    fn render_status_row(app: &AppState, width: u16) -> String {
        use ratatui::{backend::TestBackend, Terminal};

        let mut terminal = Terminal::new(TestBackend::new(width, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(app, frame, Rect::new(0, 0, width, 1)))
            .unwrap();
        terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect()
    }

    /// Two providers with known windows: Claude half-spent on the week, Codex
    /// idle. Fixed values so the glyph a percentage maps to is asserted, not
    /// assumed.
    fn usage_fixture() -> crate::provider_usage::ProviderUsageSnapshot {
        use crate::provider_usage::{AccountUsage, ProviderUsageSnapshot, QuotaWindow};
        ProviderUsageSnapshot::with_primary_accounts(
            AccountUsage {
                account: Some("SHQ".into()),
                five_hour: Some(QuotaWindow {
                    used_percent: 6,
                    resets_at: Some(2_000_000_000),
                }),
                seven_day: Some(QuotaWindow {
                    used_percent: 56,
                    resets_at: Some(2_000_100_000),
                }),
                credits: None,
                stale: false,
                ..AccountUsage::default()
            },
            AccountUsage {
                account: Some("SHQ".into()),
                seven_day: Some(QuotaWindow {
                    used_percent: 19,
                    resets_at: Some(2_000_100_000),
                }),
                ..AccountUsage::default()
            },
            AccountUsage {
                seven_day: Some(QuotaWindow {
                    used_percent: 24,
                    resets_at: Some(2_000_100_000),
                }),
                ..AccountUsage::default()
            },
        )
    }

    fn metrics(cpu: u8, mem_used: f32, disk: u8) -> StatusMetrics {
        StatusMetrics {
            cpu_percent: Some(cpu),
            cpu_cores: Some(12),
            mem_used_gib: Some(mem_used),
            mem_total_gib: Some(32.0),
            disk_percent: Some(disk),
            disk_used_gib: Some(f32::from(disk) / 100.0 * 480.0),
            disk_total_gib: Some(480.0),
            hostname: "metric-host".into(),
        }
    }

    /// An app whose last sample is `metrics`, with the visibility state the
    /// sampler would have derived from it.
    fn app_with_metrics(metrics: StatusMetrics) -> AppState {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.status_cpu_visible = metric_segment_visible(metrics.cpu_percent, false);
        app.status_mem_visible = metric_segment_visible(memory_percent(&metrics), false);
        app.status_disk_visible = metric_segment_visible(metrics.disk_percent, false);
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics,
            sampled_at: std::time::Instant::now(),
        });
        app
    }

    fn segment_text(app: &AppState) -> String {
        status_segments(app, metrics_or_unavailable(app), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect()
    }

    #[test]
    fn healthy_machine_shows_only_green_metric_icons() {
        let app = app_with_metrics(metrics(31, 19.8, 41));
        let segments = status_segments(&app, metrics_or_unavailable(&app), &app.palette);
        assert_eq!(
            segments
                .iter()
                .map(|segment| segment.kind)
                .collect::<Vec<_>>(),
            vec![
                StatusSegmentKind::Cpu,
                StatusSegmentKind::Memory,
                StatusSegmentKind::Disk,
                StatusSegmentKind::More
            ]
        );
        assert_eq!(segment_text(&app), " ●  ●  ●  ... ");
        assert!(segments[..3]
            .iter()
            .all(|segment| segment.style.fg == Some(app.palette.green)));
    }

    #[test]
    fn each_metric_appears_amber_at_80_and_red_at_90() {
        // Disk at 84% is amber; healthy metrics stay as green icons.
        let app = app_with_metrics(metrics(31, 19.8, 84));
        let segments = status_segments(&app, metrics_or_unavailable(&app), &app.palette);
        let kinds = segments
            .iter()
            .map(|segment| segment.kind)
            .collect::<Vec<_>>();
        assert_eq!(
            kinds,
            vec![
                StatusSegmentKind::Cpu,
                StatusSegmentKind::Memory,
                StatusSegmentKind::Disk,
                StatusSegmentKind::More
            ]
        );
        assert_eq!(segments[2].style.fg, Some(app.palette.yellow));
        assert_eq!(segments[2].text, " DSK 84% ");

        // Memory 93% and disk 84%: red then amber, in CPU, memory, disk order.
        let app = app_with_metrics(metrics(31, 29.8, 84));
        let segments = status_segments(&app, metrics_or_unavailable(&app), &app.palette);
        assert_eq!(
            segments
                .iter()
                .map(|segment| (segment.kind, segment.style.fg))
                .collect::<Vec<_>>(),
            vec![
                (StatusSegmentKind::Cpu, Some(app.palette.green)),
                (StatusSegmentKind::Memory, Some(app.palette.red)),
                (StatusSegmentKind::Disk, Some(app.palette.yellow)),
                (StatusSegmentKind::More, Some(app.palette.overlay1)),
            ]
        );

        // CPU at 88% uses the amber alert color.
        let mut app = app_with_metrics(metrics(88, 19.8, 41));
        app.nerd_font = true;
        let segments = status_segments(&app, metrics_or_unavailable(&app), &app.palette);
        assert_eq!(segments[0].kind, StatusSegmentKind::Cpu);
        assert_eq!(segments[0].text, " CPU 88% ");
        assert_eq!(segments[0].style.fg, Some(app.palette.yellow));
    }

    #[test]
    fn threshold_hysteresis_does_not_expand_green_range_metrics() {
        assert!(!metric_segment_visible(Some(79), false));
        assert!(metric_segment_visible(Some(80), false));
        // Between 78 and 80 the segment keeps whatever state it had, so a
        // value hovering at the threshold does not flicker every sample.
        assert!(metric_segment_visible(Some(79), true));
        assert!(metric_segment_visible(Some(78), true));
        assert!(!metric_segment_visible(Some(77), true));
        assert!(!metric_segment_visible(None, true));

        let mut app = app_with_metrics(metrics(79, 25.0, 79));
        assert!(!segment_text(&app).contains("CPU 79%"));
        assert_eq!(
            app.palette.green,
            status_segments(&app, metrics_or_unavailable(&app), &app.palette)[0]
                .style
                .fg
                .unwrap()
        );
        app.status_cpu_visible = true;
        app.status_mem_visible = true;
        app.status_disk_visible = true;
        assert!(segment_text(&app).contains("DSK 79%"));
    }

    #[test]
    fn unsampled_metrics_keep_placeholders() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.status_cpu_visible = true;
        app.status_mem_visible = true;
        app.status_disk_visible = true;
        assert_eq!(
            status_segments(&app, &StatusMetrics::default(), &app.palette)
                .iter()
                .map(|segment| segment.kind)
                .collect::<Vec<_>>(),
            vec![
                StatusSegmentKind::Cpu,
                StatusSegmentKind::Memory,
                StatusSegmentKind::Disk,
                StatusSegmentKind::More
            ]
        );
    }

    #[test]
    fn metric_tooltips_give_relative_and_absolute_values() {
        let app = app_with_metrics(metrics(88, 19.8, 84));
        assert_eq!(
            crate::ui::tooltip::status_segment_tooltip(&app, StatusSegmentKind::Cpu),
            "CPU 88% — 10.6 of 12 cores busy"
        );
        assert_eq!(
            crate::ui::tooltip::status_segment_tooltip(&app, StatusSegmentKind::Memory),
            "Memory 62% — 19.8 GB of 32 GB used · 12.2 GB free"
        );
        assert_eq!(
            crate::ui::tooltip::status_segment_tooltip(&app, StatusSegmentKind::Disk),
            "Disk 84% — 403 GB of 480 GB used on / · 77 GB free"
        );
        let more = crate::ui::tooltip::status_segment_tooltip(&app, StatusSegmentKind::More);
        assert!(more.starts_with("More\n"), "{more}");
        assert!(more.contains("Esc closes"), "{more}");
    }

    #[test]
    fn narrowest_desktop_bar_fits_every_metric_expanded_and_the_more_control() {
        let mut app = app_with_metrics(metrics(100, 32.0, 100));
        app.status_bar_expanded = true;
        let width = minimum_required_status_width(&app);
        let segments = fit_status_segments(
            status_segments(&app, metrics_or_unavailable(&app), &app.palette),
            width,
        );
        assert_eq!(segments.len(), 4);
        assert!(segment_width(&segments) <= width);

        let rendered = render_status_row(&app, u16::try_from(width).unwrap());
        for label in ["C 100%", "M 32.0/32G", "D 100%", "..."] {
            assert!(rendered.contains(label), "{label}: {rendered}");
        }
    }

    #[test]
    fn view_icons_sit_at_the_left_edge_in_a_fixed_order_with_the_view_highlighted() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        let buttons = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(
            buttons
                .iter()
                .map(|button| (button.rect.x, button.label.as_str(), button.active))
                .collect::<Vec<_>>(),
            vec![
                (0, " H ", true),
                (3, " F ", false),
                (6, " O ", false),
                (9, " S ", false),
                (12, " M ", false),
            ]
        );
        assert_eq!(
            buttons
                .iter()
                .map(|button| button.action)
                .collect::<Vec<_>>(),
            TopBarView::ALL
                .into_iter()
                .map(StatusButtonAction::View)
                .collect::<Vec<_>>()
        );

        app.open_focus_view();
        let buttons = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(
            buttons
                .iter()
                .filter(|button| button.active)
                .map(|button| button.action)
                .collect::<Vec<_>>(),
            vec![StatusButtonAction::View(TopBarView::Focus)]
        );

        app.nerd_font = true;
        let labels = status_buttons(&app, Rect::new(0, 0, 120, 1))
            .into_iter()
            .map(|button| button.label)
            .collect::<Vec<_>>();
        assert_eq!(
            labels,
            [
                " \u{F06A1} ",
                " \u{F04FE} ",
                " \u{F199C} ",
                " \u{F0CB6} ",
                " \u{F1274} "
            ]
        );
    }

    #[test]
    fn the_bar_renders_view_and_focused_title_with_always_visible_metrics() {
        let mut app = app_with_metrics(metrics(31, 19.8, 41));
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        for width in [80, 120, 200] {
            let rendered = render_status_row(&app, width);
            assert!(rendered.starts_with(" H  F  O  S  M "), "{rendered:?}");
            assert!(rendered.contains("Home"), "{rendered:?}");
            assert!(rendered.matches('\u{25cf}').count() >= 3, "{rendered:?}");
        }

        app.open_focus_view();
        for width in [80, 120, 200] {
            let rendered = render_status_row(&app, width);
            assert!(rendered.contains("Focus"), "{rendered:?}");
            assert!(!rendered.contains("Home"), "{rendered:?}");
            assert!(rendered.matches('\u{25cf}').count() >= 3, "{rendered:?}");
        }
    }

    #[test]
    fn the_view_name_uses_the_muted_colour_in_normal_weight() {
        use ratatui::{backend::TestBackend, Terminal};

        let app = app_with_metrics(metrics(31, 19.8, 41));
        let mut terminal = Terminal::new(TestBackend::new(80, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, 80, 1)))
            .unwrap();
        let cell = &terminal.backend().buffer()[((80 - 4) / 2, 0)];
        assert_eq!(cell.symbol(), "H");
        assert_eq!(cell.fg, app.palette.overlay1);
        assert!(!cell.modifier.contains(Modifier::BOLD));
    }

    #[test]
    fn view_names_follow_the_surface_on_screen() {
        let mut app = AppState::test_new();
        assert_eq!(app.top_bar_view(), TopBarView::Home);
        app.open_focus_view();
        assert_eq!(app.top_bar_view(), TopBarView::Focus);
        app.close_top_bar_views();
        app.scratch.open_writer();
        assert_eq!(app.top_bar_view(), TopBarView::Scratch);
        app.close_top_bar_views();
        app.toggle_inbox();
        assert_eq!(app.top_bar_view(), TopBarView::Mailbox);
        app.close_top_bar_views();
        assert_eq!(app.top_bar_view(), TopBarView::Home);
        assert_eq!(
            TopBarView::ALL.map(TopBarView::name),
            ["Home", "Focus", "Orchestration", "Scratch", "Mailbox"]
        );
    }

    #[test]
    fn view_icon_tooltips_name_the_view_the_click_and_the_shortcut() {
        let app = AppState::test_new();
        let tooltips = TopBarView::ALL
            .into_iter()
            .map(|view| view_button_tooltip(&app, view))
            .collect::<Vec<_>>();
        for (tooltip, (name, shortcut)) in tooltips.iter().zip([
            ("Home", "prefix+a"),
            ("Focus", "prefix+ctrl+f"),
            ("Orchestration", "prefix+ctrl+o"),
            ("Scratch", "prefix+ctrl+s"),
            ("Mailbox", "prefix+m"),
        ]) {
            let lines = tooltip.lines().collect::<Vec<_>>();
            assert_eq!(lines.len(), 3, "{tooltip}");
            assert_eq!(lines[0], name);
            assert!(!lines[1].is_empty(), "{tooltip}");
            assert_eq!(lines[2], shortcut, "{tooltip}");
        }
        assert!(tooltips[0].contains("Click to return here from any view"));
        assert!(tooltips[1].contains("Esc returns Home"));
    }

    #[test]
    fn icons_yield_whole_to_the_metrics_rather_than_overlapping_them() {
        let mut app = app_with_metrics(metrics(100, 32.0, 100));
        app.status_bar_expanded = true;
        let width = u16::try_from(segment_width(&status_segments(
            &app,
            metrics_or_unavailable(&app),
            &app.palette,
        )))
        .unwrap()
            + 7;
        let area = Rect::new(0, 0, width, 1);
        let buttons = status_buttons(&app, area);
        let segments = fitted_status_segments(&app, area);
        let segments_start = width - u16::try_from(segment_width(&segments)).unwrap();
        assert_eq!(buttons.len(), 2);
        for button in &buttons {
            assert_eq!(button.rect.width, 3, "icons drop whole, never truncate");
            assert!(button.rect.right() <= segments_start);
        }
        assert!(status_buttons(&app, Rect::new(0, 0, 0, 1)).is_empty());
    }

    #[test]
    fn hidden_indicators_live_in_the_more_popover() {
        let mut app = app_with_focused_attached_pane();
        app.nerd_font = false;
        app.machines.push(crate::app::machines::Machine {
            name: "workbox".into(),
            icon: None,
            target: Some("workbox".into()),
            socket: None,
        });
        let screen = Rect::new(0, 0, 120, 30);
        let bar = Rect::new(0, 0, 120, 1);
        assert!(status_more_layout(&app, bar, screen).is_none());

        app.status_more_open = true;
        let layout = status_more_layout(&app, bar, screen).expect("open popover");
        let rows = layout
            .rows
            .iter()
            .map(|row| {
                row.iter()
                    .map(|item| item.text.as_str())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert_eq!(rows[0], "\u{25cf} Link online");
        assert!(rows.contains(&"\u{2192} workbox".to_string()), "{rows:?}");
        assert!(rows.contains(&"Host TESTHOST".to_string()), "{rows:?}");
        assert!(rows.contains(&"⧉ Fleet shown".to_string()), "{rows:?}");
        assert!(
            rows.iter().any(|row| row.starts_with("\u{25cf}workbox")),
            "{rows:?}"
        );
        assert_eq!(
            rows.last().map(String::as_str),
            Some("▸ Metric numbers off")
        );

        // Anchored under the right edge of the bar, every item clickable.
        assert_eq!(layout.rect.y, 1);
        assert_eq!(layout.rect.right(), 120);
        for kind in [
            StatusSegmentKind::Link,
            StatusSegmentKind::RemoteHost,
            StatusSegmentKind::Hostname,
            StatusSegmentKind::FleetLabel,
            StatusSegmentKind::FleetHost(0),
            StatusSegmentKind::FleetUseMachine(0),
            StatusSegmentKind::StatusDetail,
        ] {
            let rect = layout
                .hit_areas
                .iter()
                .find_map(|(candidate, rect)| (*candidate == kind).then_some(*rect))
                .unwrap_or_else(|| panic!("{kind:?} has a hit area"));
            assert!(rect.x > layout.rect.x && rect.right() < layout.rect.right());
            assert!(rect.y > layout.rect.y && rect.bottom() < layout.rect.bottom());
        }

        // The bar itself carries none of them.
        let rendered = render_status_row(&app, 120);
        for hidden in ["TESTHOST", "workbox"] {
            assert!(!rendered.contains(hidden), "{hidden}: {rendered:?}");
        }
        assert_eq!(rendered.matches('\u{25cf}').count(), 3, "{rendered:?}");
    }

    #[test]
    fn fallback_pane_and_action_controls_are_grouped_in_the_more_popover() {
        let mut app = AppState::test_new();
        app.mouse_capture = true;
        app.view.status_bar_rect = Rect::new(0, 0, 100, 1);
        app.view.pane_toggle_below_hit_area = Rect::new(90, 0, 3, 1);
        app.view.pane_toggle_right_hit_area = Rect::new(93, 0, 3, 1);
        app.view.repo_editor_button_hit_area = Rect::new(70, 0, 3, 1);
        app.view.add_action_button_hit_area = Rect::new(73, 0, 3, 1);
        app.view.git_menu_button_hit_area = Rect::new(80, 0, 6, 1);
        let rows = status_more_rows(&app, &app.palette);
        assert!(rows.iter().flatten().any(|item| item.text == "Panes"));
        assert!(rows.iter().flatten().any(|item| item.text == "Actions"));
        for label in [
            "Split below",
            "Split right",
            "Open editor",
            "New action",
            "Git actions",
        ] {
            assert!(
                rows.iter().flatten().any(|item| item.text == label),
                "{label}"
            );
        }
    }

    #[test]
    fn the_open_popover_renders_over_the_screen_below_the_bar() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.status_more_open = true;
        app.view.status_bar_rect = Rect::new(0, 0, 60, 1);
        let mut terminal = Terminal::new(TestBackend::new(60, 10)).unwrap();
        terminal
            .draw(|frame| render_status_more_menu(&app, frame))
            .unwrap();
        let buffer = terminal.backend().buffer();
        let rows = (0..10)
            .map(|y| {
                (0..60)
                    .map(|x| buffer[(x, y)].symbol().to_string())
                    .collect::<String>()
            })
            .collect::<Vec<_>>();
        assert!(rows[0].trim().is_empty(), "{rows:#?}");
        assert!(rows[2].contains("Link online"), "{rows:#?}");
        assert!(rows[3].contains("Host TESTHOST"), "{rows:#?}");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_popover_agent_row_carries_the_blocked_count() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("agents")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        {
            let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
            terminal.detected_agent = Some(crate::detect::Agent::Claude);
            terminal.set_raw_agent_state_for_test(AgentState::Working);
        }
        let agents_row = |app: &AppState| {
            status_more_rows(app, &app.palette)
                .into_iter()
                .flatten()
                .find(|item| item.kind == StatusSegmentKind::Agents)
                .map(|item| (item.text, item.style.fg))
        };
        assert_eq!(
            agents_row(&app),
            Some((
                format!("{DOT} 1 agents · 0 waiting"),
                Some(app.palette.accent)
            ))
        );

        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(AgentState::Blocked);
        assert_eq!(
            agents_row(&app),
            Some((format!("{DOT} 1 agents · 1 waiting"), Some(app.palette.red)))
        );
    }

    #[test]
    fn the_link_row_colours_by_state() {
        let mut app = AppState::test_new();
        let link = |app: &AppState| {
            status_more_rows(app, &app.palette)
                .into_iter()
                .flatten()
                .find(|item| item.kind == StatusSegmentKind::Link)
                .map(|item| (item.text, item.style.fg))
                .expect("link row is always present")
        };
        assert_eq!(
            link(&app),
            (format!("{DOT} Link online"), Some(app.palette.green))
        );
        app.connectivity.observe(false);
        app.connectivity.observe(false);
        assert_eq!(
            link(&app),
            (format!("{DOT} Link offline"), Some(app.palette.red))
        );
    }

    #[test]
    fn render_fallback_only_reads_the_resolved_remote_host() {
        let mut app = app_with_focused_attached_pane();
        let terminal_id = app.workspaces[0]
            .terminal_id(app.workspaces[0].focused_pane_id().expect("focused pane"))
            .expect("focused terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .launch_argv = None;
        app.fleet_snapshot = crate::fleet::Snapshot::default();
        app.status_more_open = true;

        let rows = status_more_rows(&app, &app.palette);
        assert!(rows
            .iter()
            .flatten()
            .any(|item| item.text == "\u{2192} workbox"));
    }

    #[test]
    fn focused_remote_proxy_names_its_cached_host() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("focused terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .remote_proxy_host = Some("buildbox".into());
        let (runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            pane_id,
            24,
            120,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(terminal_id, runtime);

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &runtimes,
            Rect::new(0, 0, 120, 20),
        );
        assert_eq!(app.view.focused_remote_host.as_deref(), Some("buildbox"));
    }

    #[test]
    fn compute_view_registers_popover_hit_areas_only_while_open() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let screen = Rect::new(0, 0, 120, 30);
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &TerminalRuntimeRegistry::new(),
            screen,
        );
        assert_eq!(app.view.status_more_rect, Rect::default());
        assert!(!app
            .view
            .status_segment_hit_areas
            .iter()
            .any(|(kind, _)| *kind == StatusSegmentKind::Hostname));

        app.status_more_open = true;
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &TerminalRuntimeRegistry::new(),
            screen,
        );
        assert_ne!(app.view.status_more_rect, Rect::default());
        let (_, host) = app
            .view
            .status_segment_hit_areas
            .iter()
            .find(|(kind, _)| *kind == StatusSegmentKind::Hostname)
            .copied()
            .expect("host row hit area");
        assert_eq!(
            app.status_segment_kind_at(host.x, host.y),
            Some(StatusSegmentKind::Hostname)
        );
        assert!(app
            .view
            .status_segment_hit_areas
            .iter()
            .any(|(kind, rect)| *kind == StatusSegmentKind::More && rect.y == 0));
    }

    #[test]
    fn status_renderer_source_contract_excludes_io_and_sampling() {
        let source = include_str!("status.rs")
            .split_once("#[cfg(test)]\nmod tests")
            .expect("status renderer source")
            .0;
        for forbidden in [
            "std::fs",
            "std::net",
            "std::process",
            "Command::new",
            "sample_status_metrics",
            "http://",
            "https://",
        ] {
            assert!(!source.contains(forbidden), "found {forbidden}");
        }
    }

    #[test]
    fn provider_usage_never_reaches_the_bar_or_the_popover() {
        let mut app = app_with_metrics(metrics(31, 19.8, 41));
        app.provider_usage = usage_fixture();
        app.status_more_open = true;
        let mut rendered = render_status_row(&app, 120);
        rendered.extend(
            status_more_rows(&app, &app.palette)
                .into_iter()
                .flatten()
                .map(|item| item.text),
        );
        for provider in ["CC", "CX", "KI", "AG", "\u{EC82}"] {
            assert!(!rendered.contains(provider), "{provider}: {rendered}");
        }
    }
}
