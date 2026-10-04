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
        CopyFeedback, Palette, StatusButton, StatusButtonAction, StatusSegmentKind, StatusWorkLink,
        ToastKind, ToastNotification,
    },
    app::AppState,
    config::{StatusIndicatorStyle, ToastClipboardPosition, ToastHerdrPosition},
    detect::AgentState,
    platform::status_metrics::StatusMetrics,
    terminal::TerminalRuntimeRegistry,
};

/// Full-width, right-aligned top status row.
///
/// Contents, left to right: provider quota (Claude, Codex, Kimi, Antigravity) · link dot ·
/// agent dot · remote device (only when the focused pane is attached to a
/// fleet host) · device · CPU · memory · disk. The row before the first
/// surviving segment is intentionally blank.
///
/// Every value is a filled column because the question the row answers is "is
/// anything close to its ceiling", not "what is the exact number". Expanded
/// mode adds the numbers and the reset times back.
///
/// Layout: spans the full client width above the sidebar and pads before the
/// first surviving segment. On narrow widths Antigravity, Kimi, Codex, Claude,
/// then the remote device and device elide in that order; CPU and memory remain
/// required.
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
    render_focused_pane_title(app, frame, area, used + usize::from(reserved));
    for link in &app.view.status_work_links {
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(
                link.label.clone(),
                Style::default().fg(p.accent).bg(p.panel_bg),
            ))),
            link.rect,
        );
    }

    for button in &app.view.status_buttons {
        let style = if button.active {
            Style::default().fg(p.accent).bg(p.panel_bg)
        } else {
            Style::default()
                .fg(p.overlay0)
                .bg(p.panel_bg)
                .add_modifier(Modifier::DIM)
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

/// The focused repository and thread, rendered in the status row's otherwise
/// empty middle. It starts at the sidebar's right edge so the title lines up
/// with the column the pane itself occupies below, and it yields to both the
/// left-hand buttons and the right-aligned segments rather than overlapping.
fn render_focused_pane_title(app: &AppState, frame: &mut Frame, area: Rect, segments_used: usize) {
    let Some(layout) = focused_pane_title_layout(app, area, segments_used) else {
        return;
    };
    let style = Style::default()
        .fg(app.palette.subtext0)
        .bg(app.palette.panel_bg);
    let mut spans = Vec::with_capacity(2);
    if let Some(color) = layout.agent_dot_color {
        spans.push(Span::styled(
            "● ",
            Style::default().fg(color).bg(app.palette.panel_bg),
        ));
    }
    spans.push(Span::styled(layout.text, style));
    frame.render_widget(Paragraph::new(Line::from(spans)), layout.rect);
}

/// Blank columns between the title and the first link, and between links.
const WORK_LINK_GAP: usize = 2;

struct TitleLayout {
    text: String,
    rect: Rect,
    links: Vec<StatusWorkLink>,
    agent_dot_color: Option<Color>,
}

/// Whether a lowercased title already names this link, as a whole word rather
/// than as any substring.
fn title_names(lowercased_title: &str, label: &str) -> bool {
    let label = label.to_lowercase();
    let boundary = |character: char| !character.is_alphanumeric() && character != '-';
    lowercased_title
        .match_indices(&label)
        .any(|(index, matched)| {
            let before = lowercased_title[..index].chars().next_back();
            let after = lowercased_title[index + matched.len()..].chars().next();
            before.is_none_or(boundary) && after.is_none_or(boundary)
        })
}

/// The right-aligned segments this frame will draw. Built once during view
/// computation because the title and its links are laid out against them, and
/// building them twice would repeat the per-pane agent scan behind the dots.
pub(crate) fn fitted_status_segments(app: &AppState, area: Rect) -> Vec<Segment> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let reserved = super::tabs::tab_action_status_bar_reserved_width(app, area);
    let content_width = area.width.saturating_sub(reserved);
    if usize::from(content_width) < minimum_required_status_width(app) {
        return Vec::new();
    }
    status_row_segments(
        app,
        metrics_or_unavailable(app),
        &app.palette,
        area,
        content_width,
    )
}

/// Ticket and pull-request links of the focused pane, as rendered in the status
/// row. Computed during view computation so the hit areas belong to the same
/// frame as the labels, and empty whenever the title itself does not fit.
pub(crate) fn status_work_links(app: &AppState, area: Rect) -> Vec<StatusWorkLink> {
    if app.dock_context_objects.is_empty() {
        return Vec::new();
    }
    let reserved = super::tabs::tab_action_status_bar_reserved_width(app, area);
    if usize::from(area.width.saturating_sub(reserved)) < minimum_required_status_width(app) {
        return Vec::new();
    }
    let segments_used = segment_width(&app.view.status_segments) + usize::from(reserved);
    focused_pane_title_layout(app, area, segments_used)
        .map(|layout| layout.links)
        .unwrap_or_default()
}

/// Title text, its rect, and the links that follow it.
///
/// The links are paid for before the title is fitted: an identifier the human
/// clicks is worth more than the tail of a thread name they are already
/// reading. Links that would leave no room for the repository are dropped
/// rather than shown over a clipped title.
fn focused_pane_title_layout(
    app: &AppState,
    area: Rect,
    segments_used: usize,
) -> Option<TitleLayout> {
    let (repo, thread) = focused_pane_title_parts(app)?;
    let start = focused_pane_title_start(app, area);
    let segments_start = area
        .x
        .saturating_add(area.width)
        .saturating_sub(u16::try_from(segments_used).unwrap_or(u16::MAX));
    let raw_width = segments_start.saturating_sub(start);
    // Keep a separator when space permits, but at 80 columns the repository is
    // more useful than a blank cell.
    let width = usize::from(
        raw_width.saturating_sub(u16::from(usize::from(raw_width) > display_width(&repo))),
    );
    let agent_dot_color = focused_agent_dot_color(app);
    let dot_width = if agent_dot_color.is_some() { 2usize } else { 0 };
    let width = width.saturating_sub(dot_width);
    let mut links_width = 0usize;
    let mut kept: Vec<(crate::app::state::DockObjectRef, String)> = Vec::new();
    let title_of_record = fit_focused_pane_title(&repo, &thread, usize::MAX)
        .unwrap_or_default()
        .to_lowercase();
    for object in &app.dock_context_objects {
        let label = app.dock_object_label(object);
        if label.is_empty() {
            continue;
        }
        // The title already carries the ticket it was derived from. Naming it
        // twice on one row buys nothing. Matched on whole words, so `#159`
        // is not swallowed by a title that happens to mention `#1592`.
        if title_names(&title_of_record, &label) {
            continue;
        }
        let cost = WORK_LINK_GAP + display_width(&label);
        if width.saturating_sub(links_width + cost) < display_width(&repo) {
            break;
        }
        links_width += cost;
        kept.push((object.clone(), label));
    }
    let title_width = width.saturating_sub(links_width);
    let text = fit_focused_pane_title(&repo, &thread, title_width)?;
    let title_start = start.saturating_add(u16::try_from(dot_width).unwrap_or(u16::MAX));
    let mut cursor = title_start.saturating_add(display_width_u16(&text));
    let links = kept
        .into_iter()
        .map(|(object, label)| {
            let label_width = display_width_u16(&label);
            let rect = Rect::new(
                cursor.saturating_add(u16::try_from(WORK_LINK_GAP).unwrap_or(u16::MAX)),
                area.y,
                label_width,
                1,
            );
            cursor = rect.x.saturating_add(label_width);
            StatusWorkLink {
                rect,
                label,
                object,
            }
        })
        .collect();
    Some(TitleLayout {
        text,
        rect: Rect::new(
            start,
            area.y,
            u16::try_from(title_width.saturating_add(dot_width)).unwrap_or(u16::MAX),
            1,
        ),
        links,
        agent_dot_color,
    })
}

fn focused_agent_dot_color(app: &AppState) -> Option<Color> {
    let workspace = app.workspaces.get(app.active?)?;
    let pane_id = workspace.focused_pane_id()?;
    let pane = workspace.active_tab()?.panes.get(&pane_id)?;
    let terminal = app.terminals.get(workspace.terminal_id(pane_id)?)?;
    terminal.effective_known_agent()?;
    let (state, seen) = terminal.sidebar_projection(pane.seen);
    Some(state_label_color(state, seen, &app.palette))
}

/// The right-aligned segments, fitted to whatever the title left them.
fn status_row_segments(
    app: &AppState,
    metrics: &StatusMetrics,
    p: &Palette,
    area: Rect,
    content_width: u16,
) -> Vec<Segment> {
    let title_segment_budget = focused_pane_title_parts(app).and_then(|(repo, _)| {
        let start = focused_pane_title_start(app, area);
        let dot_width = u16::from(focused_agent_dot_color(app).is_some()) * 2;
        let budget = area
            .x
            .saturating_add(content_width)
            .saturating_sub(start)
            .saturating_sub(dot_width)
            .saturating_sub(display_width_u16(&repo));
        (usize::from(budget) >= minimum_title_companion_width()).then_some(usize::from(budget))
    });
    match title_segment_budget {
        Some(budget) => fitted_segments_beside_title(status_segments(app, metrics, p), budget),
        None => fitted_segments(status_segments(app, metrics, p), usize::from(content_width)),
    }
}

fn focused_pane_title_start(app: &AppState, area: Rect) -> u16 {
    let sidebar = app.view.sidebar_rect;
    let buttons_end = app
        .view
        .status_buttons
        .iter()
        .map(|button| button.rect.x.saturating_add(button.rect.width))
        .max()
        .unwrap_or(area.x);
    sidebar
        .x
        .saturating_add(sidebar.width)
        .max(area.x)
        .max(buttons_end)
}

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
    // The sidebar row for this pane reads the same function, so the two
    // surfaces cannot name one session differently.
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
    let minimum_truncated = format!("{repo} / …");
    if display_width(&minimum_truncated) > width {
        return Some(repo.to_string());
    }
    Some(truncate_end(&full, width))
}

/// Left-aligned quick-access buttons. The status bar's own segments are
/// right-aligned, so the left half is otherwise pure padding.
///
/// The blocked button carries its count so the filter state is readable without
/// opening another surface.
type StatusButtonSpec = (StatusButtonAction, String, bool);

fn status_button_specs(app: &AppState, blocked: usize, attention: usize) -> [StatusButtonSpec; 6] {
    [
        (
            StatusButtonAction::Home,
            " home ".to_string(),
            app.home.is_some(),
        ),
        (
            StatusButtonAction::Work,
            " ⑂ ".to_string(),
            app.work_view.is_some(),
        ),
        (
            StatusButtonAction::BlockedFilter,
            if blocked > 0 {
                format!(" blocked {blocked} ")
            } else {
                " blocked ".to_string()
            },
            app.blocked_filter,
        ),
        (
            StatusButtonAction::Attention,
            if attention > 0 {
                format!(" attention {attention} ")
            } else {
                " attention ".to_string()
            },
            app.home.is_some() && attention > 0,
        ),
        (
            StatusButtonAction::Dock,
            " dock ".to_string(),
            !app.dock_collapsed,
        ),
        (
            StatusButtonAction::Scratch,
            if app.nerd_font {
                " ✎ ".into()
            } else {
                " scratch ".into()
            },
            app.scratch.open,
        ),
    ]
}

pub(crate) fn status_buttons(app: &AppState, area: Rect) -> Vec<StatusButton> {
    if area.width == 0 || area.height == 0 {
        return Vec::new();
    }
    let (blocked, attention) = crate::ui::sidebar::all_agent_panel_entries(app)
        .into_iter()
        .fold((0usize, 0usize), |(blocked, attention), entry| {
            let tier = crate::ui::sidebar::entry_attention_tier(&entry);
            let is_blocked = tier == crate::terminal::state::AttentionTier::Blocked;
            if is_blocked {
                (blocked + 1, attention)
            } else if tier == crate::terminal::state::AttentionTier::Attention {
                (blocked, attention + 1)
            } else {
                (blocked, attention)
            }
        });
    let specs = status_button_specs(app, blocked, attention);

    // The right-aligned segments are load-bearing; buttons yield to them rather
    // than overlapping, and drop whole rather than truncating to an unreadable stub.
    let title_reserve = focused_pane_title_parts(app)
        .map(|(repo, _)| display_width(&repo))
        .unwrap_or(0)
        .saturating_add(usize::from(focused_agent_dot_color(app).is_some()) * 2);
    let content_width = area
        .width
        .saturating_sub(super::tabs::tab_action_status_bar_reserved_width(app, area));
    let reserved = segment_width(&fitted_segments(
        status_segments(app, metrics_or_unavailable(app), &app.palette),
        usize::from(content_width),
    )) + title_reserve;
    let budget = usize::from(content_width).saturating_sub(reserved);
    let full_width = specs
        .iter()
        .map(|(_, label, _)| display_width(label))
        .sum::<usize>();
    let width_without_attention = specs
        .iter()
        .filter(|(action, _, _)| {
            !matches!(
                action,
                StatusButtonAction::Attention | StatusButtonAction::Scratch
            )
        })
        .map(|(_, label, _)| display_width(label))
        .sum::<usize>();
    let attention_crosses_sidebar =
        focused_pane_title_parts(app).is_some() && !app.sidebar_collapsed && {
            let sidebar_width = usize::from(
                app.sidebar_width
                    .clamp(app.sidebar_min_width, app.sidebar_max_width),
            );
            width_without_attention <= sidebar_width && full_width > sidebar_width
        };
    let omit_attention = full_width > budget || attention_crosses_sidebar;

    let mut buttons = Vec::new();
    let mut x = area.x;
    let mut used = 0usize;
    for (action, label, active) in specs {
        if omit_attention && action == StatusButtonAction::Attention {
            continue;
        }
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
            action,
            active,
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
    /// Lower values elide first; `None` is required at desktop widths.
    elide_rank: Option<u8>,
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

fn fitted_segments(mut segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    while segment_width(&segments) > width {
        let candidate = segments
            .iter()
            .enumerate()
            .filter_map(|(index, segment)| segment.elide_rank.map(|rank| (rank, index)))
            .min_by_key(|(rank, _)| *rank);
        let Some((_, index)) = candidate else {
            break;
        };
        segments.remove(index);
    }

    debug_assert!(segment_width(&segments) <= width);
    segments
}

fn fitted_segments_beside_title(mut segments: Vec<Segment>, width: usize) -> Vec<Segment> {
    if width < minimum_required_status_width_for_segments() {
        // At 80 columns the repository identity wins the last few cells. CPU
        // and memory remain; the connectivity dot returns at wider sizes.
        segments.retain(|segment| segment.text.trim() != DOT.to_string());
        if width == minimum_title_companion_width() {
            if let Some(cpu) = segments
                .iter_mut()
                .find(|segment| segment.text.starts_with(" CPU "))
            {
                cpu.text.remove(0);
            }
        }
    }
    fitted_segments(segments, width)
}

fn minimum_required_status_width_for_segments() -> usize {
    1 + display_width(" CPU \u{2588} 100 ") + display_width(" MEM \u{2588} 100 ")
}

fn minimum_title_companion_width() -> usize {
    display_width("CPU \u{2588} ") + display_width(" MEM \u{2588} ")
}

pub(crate) fn minimum_required_status_width(_app: &AppState) -> usize {
    minimum_required_status_width_for_segments()
}

/// The eight fill levels of a column glyph, from shortest to full.
const FILL_LEVELS: [char; 8] = [
    '\u{2581}', '\u{2582}', '\u{2583}', '\u{2584}', '\u{2585}', '\u{2586}', '\u{2587}', '\u{2588}',
];
const WARN_PERCENT: u8 = 80;
const CRITICAL_PERCENT: u8 = 90;
const DOT: char = '\u{25cf}';

/// Maps a percentage onto a column glyph. Zero still draws the shortest column
/// rather than a blank, so a present-but-idle account stays visibly present.
pub(crate) fn fill_glyph(percent: u8) -> char {
    let index = usize::from(percent.min(100)) * FILL_LEVELS.len() / 101;
    FILL_LEVELS[index.min(FILL_LEVELS.len() - 1)]
}

fn load_color(percent: u8, p: &Palette) -> Color {
    match percent {
        percent if percent >= CRITICAL_PERCENT => p.red,
        percent if percent >= WARN_PERCENT => p.yellow,
        _ => p.green,
    }
}

pub(crate) fn provider_style(
    usage: &crate::provider_usage::AccountUsage,
    color: Color,
    p: &Palette,
) -> Style {
    if usage.stale {
        return Style::default().fg(p.overlay0).add_modifier(Modifier::DIM);
    }
    match usage.peak_percent() {
        Some(percent) if percent >= CRITICAL_PERCENT => Style::default().fg(p.red),
        Some(percent) if percent >= WARN_PERCENT => Style::default().fg(p.yellow),
        _ => Style::default().fg(color),
    }
}

use crate::ui::icons::Metric;

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

fn status_segments(
    app: &AppState,
    metrics: &crate::platform::status_metrics::StatusMetrics,
    p: &Palette,
) -> Vec<Segment> {
    let mut out = Vec::new();
    let expanded = app.status_bar_expanded;

    out.push(Segment {
        text: format!(" {} ", if expanded { "▾" } else { "▸" }),
        style: Style::default().fg(p.overlay1),
        preserve_bg: false,
        elide_rank: None,
        kind: StatusSegmentKind::StatusDetail,
    });

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
        out.push(Segment {
            text: format!(" {}", if app.nerd_font { "\u{F013B}" } else { "⧉" }),
            style: Style::default().fg(p.overlay1),
            preserve_bg: false,
            elide_rank: Some(5),
            kind: StatusSegmentKind::FleetLabel,
        });
        if app.fleet_status {
            for (idx, device) in attention_devices.iter().enumerate() {
                let (text, style) = fleet_device_text(device, p);
                out.push(Segment {
                    text,
                    style,
                    preserve_bg: false,
                    elide_rank: Some(5),
                    kind: StatusSegmentKind::FleetDevice(idx),
                });
            }
            for (idx, host) in app
                .machines
                .iter()
                .filter(|machine| !machine.is_local())
                .enumerate()
            {
                let short = format!("{:<3}", host.name.chars().take(3).collect::<String>());
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
                out.push(Segment {
                    text: format!("  {dot}{short}  "),
                    style: Style::default().fg(color),
                    preserve_bg: false,
                    elide_rank: Some(5),
                    kind: StatusSegmentKind::FleetHost(idx),
                });
                out.push(Segment {
                    text: (if app.nerd_font { "\u{F0A9}" } else { ">" }).to_string(),
                    style: Style::default().fg(p.overlay1),
                    preserve_bg: false,
                    elide_rank: Some(5),
                    kind: StatusSegmentKind::FleetUseMachine(idx),
                });
            }
        }
    }

    // The link dot is always present: an absent dot cannot be told apart from a
    // dot that elided, so "no red" would read as "online" on exactly the narrow
    // row where the answer matters. Green online, red offline, never elided.
    let online = app.connectivity.is_online();
    out.push(Segment {
        text: format!(" {DOT} "),
        style: Style::default().fg(if online { p.green } else { p.red }),
        preserve_bg: false,
        elide_rank: None,
        kind: StatusSegmentKind::Link,
    });

    let (agents, blocked) = app.agent_dot_counts();
    if agents > 0 {
        let text = if blocked > 0 {
            format!(" {DOT} {agents}/{blocked} ")
        } else {
            format!(" {DOT} {agents} ")
        };
        out.push(Segment {
            text,
            style: Style::default().fg(if blocked > 0 { p.red } else { p.accent }),
            preserve_bg: false,
            elide_rank: Some(5),
            kind: StatusSegmentKind::Agents,
        });
    }

    // A focused pane attached to a fleet host names its machine ahead of the
    // local one: the local hostname stays next to CPU/MEM, which it still
    // labels. View computation resolves attached panes and remote-focus
    // proxies before either status layout pass reaches this function. The
    // remote segment shares the device's elide rank and sits earlier in the
    // row, so the first-minimum tie-break sheds it first: a surviving remote
    // name must never end up labeling local metrics.
    if let Some(host) = app.view.focused_remote_host.as_deref() {
        out.push(Segment {
            text: format!(" \u{2192} {host} "),
            style: Style::default().fg(p.teal),
            preserve_bg: false,
            elide_rank: Some(4),
            kind: StatusSegmentKind::RemoteHost,
        });
    }

    out.push(Segment {
        text: format!(" {} ", app.chrome_host_label),
        style: Style::default().fg(p.green),
        preserve_bg: false,
        elide_rank: Some(4),
        kind: StatusSegmentKind::Hostname,
    });

    let label = |metric| crate::ui::icons::metric_label(metric, app.nerd_font);
    out.push(metric_segment(
        StatusSegmentKind::Cpu,
        label(Metric::Cpu),
        metrics.cpu_percent,
        expanded,
        p,
    ));
    out.push(metric_segment(
        StatusSegmentKind::Memory,
        label(Metric::Mem),
        memory_percent(metrics),
        expanded,
        p,
    ));
    if crate::platform::status_metrics::disk_segment_visible(
        metrics.disk_percent,
        app.status_disk_visible,
    ) {
        out.push(metric_segment(
            StatusSegmentKind::Disk,
            label(Metric::Dsk),
            metrics.disk_percent,
            expanded,
            p,
        ));
    }
    out
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

fn metric_segment(
    kind: StatusSegmentKind,
    label: &str,
    percent: Option<u8>,
    expanded: bool,
    p: &Palette,
) -> Segment {
    let (text, color) = match percent.filter(|percent| *percent <= 100) {
        Some(percent) if expanded => (
            format!(" {label} {} {percent} ", fill_glyph(percent)),
            load_color(percent, p),
        ),
        Some(percent) => (
            format!(" {label} {} ", fill_glyph(percent)),
            load_color(percent, p),
        ),
        None => (format!(" {label} - "), p.overlay0),
    };
    Segment {
        text,
        style: Style::default().fg(color),
        preserve_bg: false,
        elide_rank: None,
        kind,
    }
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
        (AgentState::Idle, false) => p.teal,
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

pub(super) fn status_report_age_compact_label(
    reported_at: Option<std::time::Instant>,
    now: std::time::Instant,
) -> Option<String> {
    let age = now.checked_duration_since(reported_at?)?;
    (age >= std::time::Duration::from_secs(60)).then(|| {
        let minutes = age.as_secs() / 60;
        if minutes >= 60 {
            format!("{}h", minutes / 60)
        } else {
            format!("{minutes}m")
        }
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::{ToastClipboardPosition, ToastHerdrPosition};

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
            (AgentState::Idle, false, "●", palette.teal),
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
                (AgentState::Idle, false, palette.teal),
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
            status_report_age_compact_label(Some(reported_at), old).as_deref(),
            Some("1h")
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
    fn required_metrics_survive_optional_segment_elision() {
        // CPU/MEM are required while branch and device elide by rank.
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.status_focused_cwd = Some(PathBuf::from("/very/long/focused/folder"));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some("feature/very-long-branch".into());
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let width = minimum_required_status_width(&app);
        let segments = fitted_segments(status_segments(&app, &metrics, &app.palette), width);
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM ▄"));
        assert!(rendered.contains("CPU ▁"));
        assert!(!rendered.contains("feature/very-long"));
    }

    #[test]
    fn review_findings_narrow_desktop_keeps_required_metrics() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.mobile_width_threshold = 0;
        let required = minimum_required_status_width(&app) as u16;
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required - 1, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
        let mut terminal = Terminal::new(TestBackend::new(required, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, required, 1)))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("CPU ▁"), "{rendered}");
        assert!(rendered.contains("MEM ▄"), "{rendered}");
    }

    #[test]
    fn rendered_status_bar_excludes_provider_usage() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.nerd_font = true;
        app.provider_usage = usage_fixture();
        let area = Rect::new(0, 0, 120, 1);
        let mut terminal = Terminal::new(TestBackend::new(area.width, area.height)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, area))
            .unwrap();

        let buffer = terminal.backend().buffer();
        assert!(!buffer
            .content()
            .iter()
            .any(|cell| cell.symbol() == "\u{EC82}"));
    }

    #[test]
    fn review_findings_status_width_is_independent_of_metric_snapshot() {
        let mut app = AppState::test_new();
        let unavailable_width = minimum_required_status_width(&app);
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: StatusMetrics {
                cpu_percent: Some(100),
                mem_used_gib: Some(17_179_869_184.0),
                mem_total_gib: Some(17_179_869_184.0),
                disk_percent: None,
                hostname: "metric-host".into(),
            },
            sampled_at: std::time::Instant::now(),
        });

        assert_eq!(minimum_required_status_width(&app), unavailable_width);
        let required = fitted_segments(
            status_segments(
                &app,
                &app.status_metrics.as_ref().unwrap().metrics,
                &app.palette,
            ),
            unavailable_width,
        );
        assert!(segment_width(&required) <= unavailable_width);
    }

    #[test]
    fn narrow_desktop_never_truncates_required_metrics() {
        use ratatui::{backend::TestBackend, Terminal};

        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.mobile_width_threshold = 0;
        let required = minimum_required_status_width(&app) as u16;
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: StatusMetrics {
                cpu_percent: Some(100),
                mem_used_gib: Some(9_999.9),
                mem_total_gib: Some(9_999.9),
                disk_percent: None,
                hostname: "metric-host".into(),
            },
            sampled_at: std::time::Instant::now(),
        });
        assert_eq!(
            minimum_required_status_width(&app) as u16,
            required,
            "live samples must not move the desktop/mobile breakpoint"
        );
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required - 1, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Mobile);

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, required, 5),
        );
        assert_eq!(app.view.layout, crate::app::state::ViewLayout::Desktop);
        let mut terminal = Terminal::new(TestBackend::new(required, 1)).unwrap();
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, required, 1)))
            .unwrap();
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("CPU \u{2588}"), "{rendered}");
        assert!(rendered.contains("MEM \u{2588}"), "{rendered}");
        assert!(
            rendered.starts_with(' '),
            "left side must stay blank: {rendered:?}"
        );
    }

    fn assert_long_context_fits_status_row(cwd: &str, branch: &str) {
        use ratatui::{backend::TestBackend, Terminal};

        const WIDTH: usize = crate::config::DEFAULT_MOBILE_WIDTH_THRESHOLD as usize;
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.status_focused_cwd = Some(PathBuf::from(cwd));
        app.status_git_cwd = app.status_focused_cwd.clone();
        app.status_git_branch = Some(branch.into());
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let segments = fitted_segments(status_segments(&app, &metrics, &app.palette), WIDTH);
        assert!(segment_width(&segments) <= WIDTH);

        let mut terminal =
            Terminal::new(TestBackend::new(WIDTH as u16, 1)).expect("create status terminal");
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, WIDTH as u16, 1)))
            .expect("render status");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("MEM ▄"), "{rendered}");
        assert!(rendered.contains("CPU ▁"), "{rendered}");
        assert!(!rendered.contains("Herdr v"), "{rendered}");
    }

    #[test]
    fn minimum_desktop_status_fits_long_ascii_context_and_required_metrics() {
        assert_long_context_fits_status_row(
            "/a/very/long/folder/path/that/must/not/displace/required/metrics",
            "feature/a-very-long-branch-that-must-elide",
        );
    }

    #[test]
    fn minimum_desktop_status_fits_wide_context_and_required_metrics() {
        assert_long_context_fits_status_row(
            "/重要な/長い/フォルダー/必須/メトリクス",
            "機能/長いブランチ名",
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

    #[test]
    fn status_metrics_fallback_format_is_explicit() {
        // AC3/AC5: unavailable snapshots render stable units without sampling in render.
        let mut app = AppState::test_new();
        app.nerd_font = false;
        let segments = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("MEM -"), "{rendered}");
        assert!(rendered.contains("CPU -"), "{rendered}");
    }

    #[test]
    fn status_metrics_outside_bounded_display_contract_use_fallbacks() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        let baseline = status_segments(
            &app,
            &StatusMetrics {
                cpu_percent: Some(12),
                mem_used_gib: Some(8.0),
                mem_total_gib: Some(16.0),
                disk_percent: None,
                hostname: "metric-host".into(),
            },
            &app.palette,
        );
        let metrics = StatusMetrics {
            cpu_percent: Some(101),
            mem_used_gib: Some(10_000.0),
            mem_total_gib: Some(10_000.0),
            disk_percent: None,
            hostname: "metric-host".into(),
        };
        let rendered = status_segments(&app, &metrics, &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("CPU -"), "{rendered}");
        // A percentage has no magnitude ceiling, so a full volume of any size
        // is expressible; only the impossible CPU reading falls back.
        assert!(rendered.contains("MEM \u{2588}"), "{rendered}");
        let fallback = status_segments(&app, &metrics, &app.palette);
        assert_eq!(
            segment_width(&baseline),
            segment_width(&fallback),
            "metric values and fallbacks must not shift or re-elide the row"
        );
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
    fn status_elision_keeps_metrics_and_drops_optional_host_labels() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.provider_usage = usage_fixture();
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let full = status_segments(&app, &metrics, &app.palette);

        assert_eq!(
            full.iter()
                .filter_map(|segment| segment.elide_rank)
                .collect::<Vec<_>>(),
            vec![4]
        );
        let rendered = full
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        for provider in ["CC", "CX", "KI", "AG"] {
            assert!(!rendered.contains(provider), "{rendered}");
        }
        assert!(rendered.contains("TESTHOST"), "{rendered}");

        let optional_width = full
            .iter()
            .filter(|segment| segment.elide_rank.is_some())
            .map(|segment| display_width(&segment.text))
            .sum::<usize>();
        let required = fitted_segments(
            status_segments(&app, &metrics, &app.palette),
            segment_width(&full) - optional_width,
        );
        let rendered = required
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("TESTHOST"), "{rendered}");
        assert!(rendered.contains("CPU \u{2581}"), "{rendered}");
        assert!(rendered.contains("MEM \u{2584}"), "{rendered}");
    }

    #[test]
    fn configured_fleet_machine_stays_visible_without_live_inventory() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.machines.push(crate::app::machines::Machine {
            name: "workbox".into(),
            icon: None,
            target: Some("workbox".into()),
            socket: None,
        });
        let segments = status_segments(
            &app,
            &crate::platform::status_metrics::status_metrics_fixture(),
            &app.palette,
        );
        let host = segments
            .iter()
            .find(|segment| segment.kind == StatusSegmentKind::FleetHost(0))
            .expect("configured host segment");
        assert_eq!(host.text, "  ·wor  ");
        assert_eq!(host.style.fg, Some(app.palette.overlay0));
        assert!(segments
            .iter()
            .any(|segment| segment.kind == StatusSegmentKind::FleetUseMachine(0)));
    }

    #[test]
    fn antigravity_quota_is_not_rendered_in_the_status_bar() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.provider_usage = usage_fixture();
        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Agy)
            .five_hour = Some(crate::provider_usage::QuotaWindow {
            used_percent: 12,
            resets_at: Some(2_000_000_000),
        });

        let segments = status_segments(
            &app,
            &crate::platform::status_metrics::status_metrics_fixture(),
            &app.palette,
        );
        assert!(!segments.iter().any(|segment| segment.text.contains("AG")));
        assert_eq!(
            status_segments(
                &app,
                &crate::platform::status_metrics::status_metrics_fixture(),
                &app.palette,
            )
            .iter()
            .filter_map(|segment| segment.elide_rank)
            .collect::<Vec<_>>(),
            vec![4]
        );
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

    #[test]
    fn attached_focused_pane_names_the_remote_host_before_the_local_one() {
        let mut app = app_with_focused_attached_pane();
        app.nerd_font = false;
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 120, 20),
        );

        let rendered = render_status_row(&app, 120);

        let remote = rendered
            .find("\u{2192} workbox")
            .expect("remote segment renders");
        let local = rendered.find("TESTHOST").expect("local host label renders");
        assert!(
            remote < local,
            "remote segment precedes the local host label: {rendered}"
        );
        assert!(rendered.contains("CPU"), "{rendered}");
    }

    #[test]
    fn local_focused_pane_keeps_the_row_free_of_a_remote_segment() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: crate::platform::status_metrics::status_metrics_fixture(),
            sampled_at: std::time::Instant::now(),
        });
        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 120, 20),
        );

        let rendered = render_status_row(&app, 120);

        assert_eq!(app.view.focused_remote_host, None);
        assert!(rendered.contains("TESTHOST"), "{rendered}");
        assert!(!rendered.contains('\u{2192}'), "{rendered}");
    }

    #[test]
    fn focused_agent_dot_uses_the_sidebar_lifecycle_color() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let workspace = &app.workspaces[0];
        let pane_id = workspace.focused_pane_id().expect("focused pane");
        let terminal_id = workspace.terminal_id(pane_id).expect("terminal").clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal
            .set_detected_agent_process_at(crate::detect::Agent::Codex, std::time::Instant::now());
        terminal.set_raw_agent_state_for_test(AgentState::Working);

        assert_eq!(focused_agent_dot_color(&app), Some(app.palette.blue));
    }

    #[test]
    fn focused_remote_proxy_names_its_cached_host() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.active = Some(0);
        app.ensure_test_terminals();
        app.status_metrics = Some(crate::platform::status_metrics::StatusMetricsSnapshot {
            metrics: crate::platform::status_metrics::status_metrics_fixture(),
            sampled_at: std::time::Instant::now(),
        });
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
        let rendered = render_status_row(&app, 120);

        assert_eq!(app.view.focused_remote_host.as_deref(), Some("buildbox"));
        assert!(rendered.contains("\u{2192} buildbox"), "{rendered}");
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

        let rendered = render_status_row(&app, 120);

        assert!(rendered.contains("\u{2192} workbox"), "{rendered}");
    }

    #[test]
    fn narrow_width_elides_host_labels_before_metrics() {
        let mut app = app_with_focused_attached_pane();
        app.nerd_font = false;
        app.provider_usage = usage_fixture();
        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let full = status_segments(&app, &metrics, &app.palette);
        assert_eq!(
            full.iter()
                .filter_map(|segment| segment.elide_rank)
                .collect::<Vec<_>>(),
            vec![4, 4],
            "nonessential segments elide before required metrics"
        );

        let remote_width = display_width(" \u{2192} workbox ");
        let fitted = fitted_segments(
            status_segments(&app, &metrics, &app.palette),
            segment_width(&full),
        );
        let rendered = fitted
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(rendered.contains("\u{2192} workbox"), "{rendered}");

        let fitted = fitted_segments(
            status_segments(&app, &metrics, &app.palette),
            segment_width(&full) - remote_width,
        );
        let rendered = fitted
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("workbox"), "{rendered}");
        assert!(rendered.contains("TESTHOST"), "{rendered}");
        assert!(rendered.contains("CPU \u{2581}"), "{rendered}");
        assert!(rendered.contains("MEM \u{2584}"), "{rendered}");

        // The same holds through the real render path at the narrowest
        // supported width: only the required metrics survive.
        let narrow = minimum_required_status_width(&app) as u16;
        let rendered = render_status_row(&app, narrow);
        assert!(!rendered.contains("workbox"), "{rendered}");
        assert!(rendered.contains("CPU"), "{rendered}");
        assert!(rendered.contains("MEM"), "{rendered}");
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

    #[test]
    fn status_content_order_theme_and_omitted_segments_match_contract() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        let mut workspace = crate::workspace::Workspace::test_new("status");
        workspace.identity_cwd = PathBuf::from("/home/test/work/status");
        workspace.cached_git_branch = Some("feature/native-status".into());
        workspace.test_add_tab(Some("logs"));
        workspace.switch_tab(1);
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.status_git_cwd = Some(PathBuf::from("/home/test/work/status"));
        app.status_focused_cwd = app.status_git_cwd.clone();
        app.status_git_branch = Some("feature/native-status".into());
        app.provider_usage = usage_fixture();

        let metrics = crate::platform::status_metrics::status_metrics_fixture();
        let segments = status_segments(&app, &metrics, &app.palette);
        assert_eq!(
            segments.len(),
            5,
            "detail toggle, link dot, host, CPU, memory"
        );
        let rendered = segments
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        let ordered = ["▸", "\u{25cf}", "TESTHOST", "CPU \u{2581}", "MEM \u{2584}"];
        let mut previous = 0;
        for value in ordered {
            let index = rendered
                .find(value)
                .unwrap_or_else(|| panic!("{value}: {rendered}"));
            assert!(index >= previous, "{value} is out of order: {rendered}");
            previous = index;
        }
        // The branch went with the redesign: it never changed inside a
        // workspace, so it cost a column and answered nothing. Percentages and
        // reset times belong to expanded mode, not to every frame.
        for removed in [
            "feature/native-stat",
            "testuser",
            "10.0.0.2",
            "session:",
            "workspace:",
            "~/work/status",
            "Herdr v",
            "GiB",
            "56",
            "12%",
        ] {
            assert!(!rendered.contains(removed), "{removed}: {rendered}");
        }
        assert!(!rendered.contains("CC SHQ"));
        assert_eq!(
            segments
                .iter()
                .find(|segment| segment.text.contains("TESTHOST"))
                .unwrap()
                .style
                .fg,
            Some(app.palette.green)
        );
        // A load colour, not a fixed one: CPU and memory are read for how close
        // they are to their ceiling, and 12% and 50% are both far from it.
        for label in ["CPU ", "MEM "] {
            assert_eq!(
                segments
                    .iter()
                    .find(|segment| segment.text.contains(label))
                    .unwrap()
                    .style
                    .fg,
                Some(app.palette.green),
                "{label}"
            );
        }
    }

    #[test]
    fn fill_glyphs_span_the_range_without_a_blank_at_zero() {
        // Zero draws the shortest column, not nothing: an account that is
        // present and idle must look different from an account that is absent.
        assert_eq!(fill_glyph(0), '\u{2581}');
        assert_eq!(fill_glyph(50), '\u{2584}');
        assert_eq!(fill_glyph(100), '\u{2588}');
        for percent in 0..=100u8 {
            assert!(FILL_LEVELS.contains(&fill_glyph(percent)), "{percent}");
        }
        // Out-of-range input saturates rather than panicking on the index.
        assert_eq!(fill_glyph(255), '\u{2588}');
    }

    #[test]
    fn status_detail_toggle_does_not_restore_provider_usage() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.provider_usage = usage_fixture();
        app.status_now_unix = Some(1_999_990_100);

        let compact = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(compact.contains("▸"), "{compact}");
        assert!(!compact.contains("CC"), "{compact}");

        app.status_bar_expanded = true;
        let expanded = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(expanded.contains("▾"), "{expanded}");
        assert!(!expanded.contains("CC"), "{expanded}");
    }

    #[test]
    fn nearly_spent_provider_usage_stays_out_of_the_status_bar() {
        // The one moment the exact figure matters, it appears unasked.
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.provider_usage = usage_fixture();
        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Claude)
            .seven_day = Some(crate::provider_usage::QuotaWindow {
            used_percent: 94,
            resets_at: Some(1_999_999_999),
        });
        app.status_now_unix = Some(1_999_990_100);

        let rendered = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("CC"));
        assert!(!rendered.contains("94"));
    }

    #[test]
    fn provider_staleness_is_not_rendered_in_the_status_bar() {
        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.provider_usage = usage_fixture();
        app.provider_usage
            .primary_usage_mut(crate::provider_usage::QuotaProvider::Codex)
            .stale = true;

        let rendered = status_segments(&app, &StatusMetrics::default(), &app.palette)
            .iter()
            .map(|segment| segment.text.as_str())
            .collect::<String>();
        assert!(!rendered.contains("CX"));
    }

    #[test]
    fn the_link_dot_is_always_present_and_colours_by_state() {
        let mut app = AppState::test_new();
        fn bare_dot(segments: &[Segment]) -> (Option<Color>, Option<u8>) {
            let dot = segments
                .iter()
                .find(|segment| segment.text.trim() == DOT.to_string())
                .expect("link dot");
            (dot.style.fg, dot.elide_rank)
        }

        let segments = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let (fg, rank) = bare_dot(&segments);
        assert_eq!(fg, Some(app.palette.green));
        assert!(rank.is_none(), "the link must outlive the data");

        app.connectivity.observe(false);
        app.connectivity.observe(false);
        let segments = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let (fg, rank) = bare_dot(&segments);
        assert_eq!(fg, Some(app.palette.red));
        assert!(rank.is_none(), "the reason must outlive the data");
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_agent_dot_turns_red_and_carries_the_blocked_count() {
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

        let working = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let dot = working
            .iter()
            .find(|segment| segment.text.contains(DOT) && segment.text.trim() != DOT.to_string())
            .expect("agent dot");
        assert_eq!(dot.text.trim(), format!("{DOT} 1"));
        assert_eq!(dot.style.fg, Some(app.palette.accent));

        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal")
            .set_raw_agent_state_for_test(AgentState::Blocked);
        let blocked = status_segments(&app, &StatusMetrics::default(), &app.palette);
        let dot = blocked
            .iter()
            .find(|segment| segment.text.contains(DOT) && segment.text.trim() != DOT.to_string())
            .expect("agent dot");
        assert_eq!(dot.text.trim(), format!("{DOT} 1/1"));
        assert_eq!(dot.style.fg, Some(app.palette.red));
    }

    #[test]
    fn disk_appears_only_when_it_is_news_and_holds_through_the_hysteresis_gap() {
        use crate::platform::status_metrics::disk_segment_visible;

        assert!(!disk_segment_visible(Some(41), false));
        assert!(disk_segment_visible(Some(80), false));
        // Between 78 and 80 the segment keeps whatever state it had, so a
        // volume hovering at the threshold does not flicker every sample.
        assert!(disk_segment_visible(Some(79), true));
        assert!(!disk_segment_visible(Some(79), false));
        assert!(!disk_segment_visible(Some(77), true));
        assert!(!disk_segment_visible(None, true));

        let mut app = AppState::test_new();
        app.nerd_font = false;
        app.status_disk_visible = true;
        let metrics = StatusMetrics {
            disk_percent: Some(97),
            ..crate::platform::status_metrics::status_metrics_fixture()
        };
        let rendered = status_segments(&app, &metrics, &app.palette);
        let disk = rendered
            .iter()
            .find(|segment| segment.text.contains("DSK"))
            .expect("disk segment");
        assert!(disk.text.contains('\u{2588}'), "{}", disk.text);
        assert_eq!(disk.style.fg, Some(app.palette.red));
    }

    /// End-to-end evidence, run by hand: every provider, metric, and account
    /// label comes from this machine's real sources rather than a fixture.
    /// Ignored in CI, where none of those sources exist.
    #[test]
    #[ignore = "reads this machine's real usage caches"]
    fn live_smoke_renders_both_modes_from_real_sources() {
        let mut app = AppState::test_new();
        app.provider_usage = crate::provider_usage::collect(
            crate::provider_usage::now_unix(),
            std::time::Instant::now(),
        );
        app.status_now_unix = crate::provider_usage::now_unix();
        app.status_disk_visible = true;
        for account in &app.provider_usage.accounts {
            println!(
                "quota {:?} {} ({}) 5h={:?} 7d={:?} stale={}",
                account.provider,
                account.profile_id,
                account.label,
                account.usage.five_hour,
                account.usage.seven_day,
                account.usage.stale,
            );
        }

        // CPU is a delta, so it needs two samples a sampling interval apart.
        let mut sampler = crate::platform::status_metrics::StatusMetricSampler::new();
        let _ = crate::platform::sample_status_metrics(&mut sampler);
        std::thread::sleep(crate::platform::status_metrics::STATUS_METRIC_REFRESH_INTERVAL);
        let metrics = crate::platform::sample_status_metrics(&mut sampler);

        for expanded in [false, true] {
            app.status_bar_expanded = expanded;
            let row: String = status_segments(&app, &metrics, &app.palette)
                .iter()
                .map(|segment| segment.text.as_str())
                .collect();
            println!("{} |{row}", if expanded { "expanded" } else { "compact " });
        }
    }

    /// Look a button up by what it does. Indexing by position makes every test
    /// a hostage of the button order.
    fn button_for(buttons: &[StatusButton], action: StatusButtonAction) -> StatusButton {
        buttons
            .iter()
            .find(|button| button.action == action)
            .unwrap_or_else(|| panic!("no {action:?} button in {buttons:?}"))
            .clone()
    }

    #[test]
    fn quick_buttons_sit_at_the_left_edge_in_a_stable_order() {
        let app = AppState::test_new();
        let buttons = status_buttons(&app, Rect::new(0, 0, 120, 1));

        let actions: Vec<StatusButtonAction> = buttons.iter().map(|button| button.action).collect();
        assert_eq!(
            actions,
            vec![
                StatusButtonAction::Home,
                StatusButtonAction::Work,
                StatusButtonAction::BlockedFilter,
                StatusButtonAction::Attention,
                StatusButtonAction::Dock,
                StatusButtonAction::Scratch
            ]
        );
        assert_eq!(buttons[0].rect.x, 0);
        // Adjacent, never overlapping: a click can only ever hit one button.
        for pair in buttons.windows(2) {
            assert_eq!(pair[0].rect.x + pair[0].rect.width, pair[1].rect.x);
        }
    }

    #[test]
    fn status_button_specs_are_a_fixed_array() {
        let app = AppState::test_new();
        let [home, work, blocked, attention, dock, scratch] = status_button_specs(&app, 2, 3);

        assert_eq!(home.0, StatusButtonAction::Home);
        assert_eq!(work.0, StatusButtonAction::Work);
        assert_eq!(blocked.1.trim(), "blocked 2");
        assert_eq!(attention.1.trim(), "attention 3");
        assert_eq!(dock.0, StatusButtonAction::Dock);
        assert_eq!(scratch.0, StatusButtonAction::Scratch);
    }

    #[test]
    fn quick_button_counts_classify_each_agent_entry_once() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("quick")];
        app.ensure_test_terminals();
        crate::ui::sidebar::take_entry_attention_tier_visits();

        status_buttons(&app, Rect::new(0, 0, 120, 1));

        assert_eq!(
            crate::ui::sidebar::take_entry_attention_tier_visits(),
            crate::ui::all_agent_panel_entries(&app).len()
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn the_blocked_button_carries_the_blocked_count_and_lights_up_when_filtered() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("quick")];
        app.active = Some(0);
        app.ensure_test_terminals();

        let idle = status_buttons(&app, Rect::new(0, 0, 120, 1));
        let idle = button_for(&idle, StatusButtonAction::BlockedFilter);
        assert_eq!(idle.label.trim(), "blocked");
        assert!(!idle.active, "the filter starts disabled");

        let pane_id = app.workspaces[0].focused_pane_id().expect("pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("terminal")
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.set_raw_agent_state_for_test(AgentState::Working);
        terminal.apply_closing_block_payload(
            vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Gate".into(),
                text: "Approve the change".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
            Vec::new(),
        );

        let working_blocked = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(
            button_for(&working_blocked, StatusButtonAction::BlockedFilter)
                .label
                .trim(),
            "blocked 1"
        );

        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_raw_agent_state_for_test(AgentState::Blocked);

        let blocked = status_buttons(&app, Rect::new(0, 0, 120, 1));
        let blocked = button_for(&blocked, StatusButtonAction::BlockedFilter);
        assert_eq!(blocked.label.trim(), "blocked 1");
        assert!(!blocked.active);

        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.apply_closing_block_payload(
            Vec::new(),
            vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Answer".into(),
                text: "Choose one".into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }],
            Vec::new(),
        );
        let attention = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert_eq!(
            button_for(&attention, StatusButtonAction::BlockedFilter)
                .label
                .trim(),
            "blocked 1"
        );
        assert!(!attention
            .iter()
            .any(|button| button.action == StatusButtonAction::Attention));

        app.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .settled_at = Some(1);
        let settled = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert!(!settled
            .iter()
            .any(|button| button.action == StatusButtonAction::Attention));

        app.blocked_filter = true;
        let filtered = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert!(button_for(&filtered, StatusButtonAction::BlockedFilter).active);
    }

    #[test]
    fn the_dock_button_lights_up_only_while_the_dock_is_showing() {
        let mut app = AppState::test_new();
        app.dock_collapsed = true;
        let collapsed = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert!(!button_for(&collapsed, StatusButtonAction::Dock).active);
        app.dock_collapsed = false;
        let showing = status_buttons(&app, Rect::new(0, 0, 120, 1));
        assert!(button_for(&showing, StatusButtonAction::Dock).active);
    }

    #[test]
    fn buttons_yield_to_the_metrics_rather_than_overlapping_them() {
        let app = AppState::test_new();
        let narrow = Rect::new(0, 0, minimum_required_status_width(&app) as u16 + 2, 1);

        let buttons = status_buttons(&app, narrow);

        // Whole buttons drop; none is truncated into an unreadable stub.
        assert!(buttons.len() <= 2, "buttons: {buttons:?}");
        for button in &buttons {
            assert!(button.rect.width as usize == display_width(&button.label));
        }
    }

    #[test]
    fn a_zero_width_status_bar_registers_no_buttons() {
        let app = AppState::test_new();
        assert!(status_buttons(&app, Rect::new(0, 0, 0, 0)).is_empty());
    }

    #[test]
    fn focused_pane_title_starts_at_sidebar_and_keeps_repo_at_eighty_columns() {
        use ratatui::{backend::TestBackend, Terminal};

        const WIDTH: u16 = 80;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_collapsed = false;
        app.sidebar_width = 40;
        let terminal_id = app.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("root pane")
            .attached_terminal_id
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("focused terminal")
            .set_terminal_title(Some("Fix billing".into()));
        app.terminals
            .get_mut(&terminal_id)
            .expect("focused terminal")
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: Some("herdrdev/herdr".into()),
                ..Default::default()
            })
            .expect("repo context");

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, WIDTH, 20),
        );
        let sidebar_end = usize::from(
            app.view
                .sidebar_rect
                .x
                .saturating_add(app.view.sidebar_rect.width),
        );
        assert!(sidebar_end > 0, "desktop layout must expose a sidebar");

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, 1)).expect("status terminal");
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, WIDTH, 1)))
            .expect("render status");
        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();

        // The buffer mixes multi-byte glyphs, so slice by column, not by byte.
        let columns = rendered.chars().collect::<Vec<_>>();
        let at_sidebar_edge = columns[sidebar_end..].iter().collect::<String>();
        assert!(
            at_sidebar_edge.starts_with("herdr"),
            "title must start at the sidebar's right edge: {rendered:?}"
        );
        assert_eq!(
            columns[sidebar_end - 1],
            ' ',
            "nothing may run into the title column: {rendered:?}"
        );
    }

    #[test]
    fn work_links_follow_the_title_with_clickable_labels() {
        use ratatui::{backend::TestBackend, Terminal};

        const WIDTH: u16 = 120;
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.ensure_test_terminals();
        app.active = Some(0);
        app.sidebar_collapsed = false;
        app.sidebar_width = 30;
        let terminal_id = app.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("root pane")
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("focused terminal");
        terminal.set_terminal_title(Some("Fix billing".into()));
        terminal.replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
            repo: Some("herdrdev/herdr".into()),
            session_name: Some("Fix billing".into()),
            pr_urls: vec!["https://github.com/herdrdev/herdr/pull/159".into()],
            ticket_ids: vec!["SCA-3165".into()],
            ..Default::default()
        });

        crate::ui::compute_view_with_runtime_registry(
            &mut app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, WIDTH, 20),
        );

        assert!(
            app.dock_open_surfaces.is_empty(),
            "the links belong to the status row, not to an uninvited dock tab"
        );
        assert_eq!(
            app.view
                .status_work_links
                .iter()
                .map(|link| link.label.as_str())
                .collect::<Vec<_>>(),
            ["#159"],
            "the ticket is already in the title and is not repeated"
        );

        let mut terminal = Terminal::new(TestBackend::new(WIDTH, 1)).expect("status terminal");
        terminal
            .draw(|frame| render_status_bar(&app, frame, Rect::new(0, 0, WIDTH, 1)))
            .expect("render status");
        let columns = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol().to_string())
            .collect::<Vec<_>>();
        let rendered = columns.concat();
        assert!(
            rendered.contains("herdr / SCA-3165 · Fix billing  #159"),
            "links follow the title with a gap between them: {rendered:?}"
        );

        // Every label is clickable exactly where it was drawn.
        for link in &app.view.status_work_links {
            let start = usize::from(link.rect.x);
            let end = start + usize::from(link.rect.width);
            assert_eq!(columns[start..end].concat(), link.label, "{rendered:?}");
        }
    }

    #[test]
    fn a_title_names_a_link_only_on_a_whole_word() {
        assert!(title_names("sca-3165 · fix billing", "SCA-3165"));
        assert!(title_names("#159 review", "#159"));
        assert!(!title_names("#1592 review", "#159"));
        assert!(!title_names("sca-31650 · fix billing", "SCA-3165"));
    }

    #[test]
    fn the_status_row_title_is_the_title_the_sidebar_row_shows() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let terminal_id = app.workspaces[0].tabs[0]
            .panes
            .values()
            .next()
            .expect("root pane")
            .attached_terminal_id
            .clone();
        let terminal = app
            .terminals
            .get_mut(&terminal_id)
            .expect("focused terminal");
        terminal.set_terminal_title(Some("cc · herdr · Fix billing".into()));
        terminal.replace_prevalidated_manual_work_context(crate::work_context::PaneWorkContext {
            repo: Some("herdrdev/herdr".into()),
            session_name: Some("Fix billing".into()),
            ticket_ids: vec!["SCA-3165".into()],
            ..Default::default()
        });

        let sidebar_title = crate::ui::sidebar::agent_panel_entries_from(
            &app,
            &crate::terminal::TerminalRuntimeRegistry::new(),
        )
        .into_iter()
        .find(|entry| entry.local_target().unwrap().ws_idx == 0)
        .and_then(|entry| entry.primary_tab_label.clone())
        .expect("the sidebar names this session");

        assert_eq!(
            focused_pane_title_parts(&app).map(|(_, title)| title),
            Some(sidebar_title),
            "one session, one title"
        );
    }

    #[test]
    fn focused_pane_title_preserves_the_repo_then_truncates_the_thread() {
        assert_eq!(
            fit_focused_pane_title("herdr", "fix a long thread name", 16).as_deref(),
            Some("herdr / fix a l…")
        );
        assert_eq!(
            fit_focused_pane_title("herdr", "fix", 5).as_deref(),
            Some("herdr")
        );
        assert_eq!(fit_focused_pane_title("herdr", "fix", 4), None);
    }

    #[test]
    fn focused_pane_title_uses_focused_context_and_cwd_fallback() {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("status")];
        app.ensure_test_terminals();
        app.active = Some(0);
        let pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("focused terminal")
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.cwd = PathBuf::from("/tmp/local-repo");
        terminal.set_manual_label("manual thread".into());

        assert_eq!(
            focused_pane_title_parts(&app),
            Some(("local-repo".into(), "manual thread".into()))
        );
    }
}

#[cfg(test)]
mod metric_icons {
    use super::*;
    use crate::ui::icons::{metric_label, Metric};

    /// Expanded mirrors `app.status_bar_expanded`: the percentage only shows
    /// when the status bar is expanded.
    fn texts(nerd_font: bool, expanded: bool) -> Vec<String> {
        let p = Palette::catppuccin();
        [Metric::Cpu, Metric::Mem, Metric::Dsk]
            .into_iter()
            .map(|metric| {
                metric_segment(
                    StatusSegmentKind::Cpu,
                    metric_label(metric, nerd_font),
                    Some(50),
                    expanded,
                    &p,
                )
                .text
            })
            .collect()
    }

    #[test]
    fn nerd_font_on_collapsed_renders_glyph_and_fill_only() {
        assert_eq!(
            texts(true, false),
            [" \u{F061A} ▄ ", " \u{F035B} ▄ ", " \u{F02CA} ▄ "]
        );
    }

    #[test]
    fn nerd_font_on_expanded_adds_percentage() {
        assert_eq!(
            texts(true, true),
            [" \u{F061A} ▄ 50 ", " \u{F035B} ▄ 50 ", " \u{F02CA} ▄ 50 "]
        );
    }

    #[test]
    fn nerd_font_off_collapsed_renders_text_and_fill_only() {
        assert_eq!(texts(false, false), [" CPU ▄ ", " MEM ▄ ", " DSK ▄ "]);
    }

    #[test]
    fn nerd_font_off_expanded_adds_percentage() {
        assert_eq!(
            texts(false, true),
            [" CPU ▄ 50 ", " MEM ▄ 50 ", " DSK ▄ 50 "]
        );
    }
}
