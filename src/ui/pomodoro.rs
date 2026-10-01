//! The break timer's two surfaces: a compact countdown in the sidebar footer and
//! the blocking overlay raised when a phase ends.
//!
//! The overlay is deliberately not dismissible by Enter alone. Its whole value
//! is that acknowledging a break costs a sentence, so an ignored reminder stays
//! on screen.

use ratatui::{
    layout::{Constraint, Layout, Rect},
    style::{Modifier, Style},
    text::{Line, Span},
    widgets::{Clear, Paragraph, Wrap},
    Frame,
};

use super::text::display_width_u16;
use super::widgets::{
    action_button_row_rects, centered_popup_rect, panel_contrast_fg, render_action_button,
    render_modal_header, render_modal_shell, render_panel_shell, ActionButtonSpec,
};
use crate::app::AppState;
use crate::pomodoro::{PomodoroPhase, PomodoroPrompt, SEND_OFF_DURATION};

/// Width of `⏱ 25:00` plus a leading space.
const INDICATOR_WIDTH: u16 = 8;
/// Subtitles toggle, five icons, refresh, board and lock, each a glyph plus a
/// blank cell so wide-rendering glyphs never touch their neighbour.
const FOOTER_ICON_COLUMNS: u16 = 18;
const NOTIFICATION_WIDTH: u16 = 2;
const CYCLE_MODE_WIDTH: u16 = 1;
const SIDEBAR_AREAS_WIDTH: u16 = 1;
const PROMPT_WIDTH: u16 = 62;
const PROMPT_HEIGHT: u16 = 12;
const ORB_PROMPT_WIDTH: u16 = 82;
const ORB_PROMPT_HEIGHT: u16 = 21;
const ORB_COLUMN_WIDTH: u16 = 37;
const ORB_CONTENT_WIDTH: u16 = 40;
const ORB_MIN_RADIUS: f64 = 3.0;
const ORB_MAX_RADIUS: f64 = 7.5;
const SEND_OFF_WIDTH: u16 = 52;
const SEND_OFF_HEIGHT: u16 = 7;
pub(crate) const ANIMATION_FRAME_INTERVAL: std::time::Duration =
    std::time::Duration::from_millis(125);

/// Pomodoro input ownership for one client. A committed frame replaces this
/// snapshot; between frames, newly presentable overlays can only add ownership.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) struct InputPresentation {
    pub(crate) area: Rect,
    pub(crate) send_off: Option<Rect>,
    pub(crate) prompt: Option<Rect>,
}

impl InputPresentation {
    pub(crate) fn owns_input(self) -> bool {
        self.send_off.is_some() || self.prompt.is_some()
    }
}
const BREAK_TIPS: [&str; 3] = [
    "stand up · look at something far away",
    "unclench your jaw · drop your shoulders",
    "get some water · leave the screen",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum BreathStage {
    Inhale,
    Hold,
    Exhale,
}

#[derive(Debug, Clone, Copy, PartialEq)]
struct BreathFrame {
    progress: f64,
    stage: BreathStage,
    cue: &'static str,
    remaining: u8,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum OrbCell {
    Fill {
        symbol: &'static str,
        color_band: u8,
    },
    Halo,
}

fn breath_at(elapsed: std::time::Duration) -> BreathFrame {
    let cycle = elapsed.as_secs_f64() % 10.0;
    let ease = |value: f64| 0.5 - 0.5 * (std::f64::consts::PI * value).cos();
    if cycle < 4.0 {
        BreathFrame {
            progress: ease(cycle / 4.0),
            stage: BreathStage::Inhale,
            cue: "breathe in",
            remaining: (4.0 - cycle).ceil().max(1.0) as u8,
        }
    } else if cycle < 5.0 {
        BreathFrame {
            progress: 1.0,
            stage: BreathStage::Hold,
            cue: "hold",
            remaining: (5.0 - cycle).ceil().max(1.0) as u8,
        }
    } else {
        BreathFrame {
            progress: 1.0 - ease((cycle - 5.0) / 5.0),
            stage: BreathStage::Exhale,
            cue: "breathe out",
            remaining: (10.0 - cycle).ceil().max(1.0) as u8,
        }
    }
}

fn animation_elapsed_at(
    now: std::time::Instant,
    started_at: std::time::Instant,
) -> std::time::Duration {
    let elapsed = now.saturating_duration_since(started_at);
    let frames = elapsed.as_nanos() / ANIMATION_FRAME_INTERVAL.as_nanos();
    let nanos = frames.saturating_mul(ANIMATION_FRAME_INTERVAL.as_nanos());
    std::time::Duration::from_nanos(u64::try_from(nanos).unwrap_or(u64::MAX))
}

fn animation_started_at(app: &AppState) -> Option<std::time::Instant> {
    app.pomodoro
        .prompt
        .as_ref()
        .map(|prompt| prompt.raised_at)
        .or_else(|| app.pomodoro.send_off.map(|send_off| send_off.shown_at))
}

fn animation_frame_at(now: std::time::Instant, started_at: std::time::Instant) -> u64 {
    now.saturating_duration_since(started_at)
        .as_nanos()
        .checked_div(ANIMATION_FRAME_INTERVAL.as_nanos())
        .and_then(|frame| u64::try_from(frame).ok())
        .unwrap_or(u64::MAX)
}

fn next_animation_frame_at(
    now: std::time::Instant,
    started_at: std::time::Instant,
) -> std::time::Instant {
    let next_frame = animation_frame_at(now, started_at).saturating_add(1);
    started_at
        + ANIMATION_FRAME_INTERVAL.saturating_mul(u32::try_from(next_frame).unwrap_or(u32::MAX))
}

fn orb_cell(dx: i32, dy: i32, radius: f64) -> Option<OrbCell> {
    let distance = ((f64::from(dx) * 0.5).powi(2) + f64::from(dy).powi(2)).sqrt();
    if distance < radius {
        let relative = distance / radius;
        let symbol = if relative < 0.28 {
            "█"
        } else if relative < 0.52 {
            "▓"
        } else if relative < 0.76 {
            "▒"
        } else {
            "░"
        };
        let color_band = if relative < 0.4 {
            0
        } else if relative < 0.72 {
            1
        } else {
            2
        };
        Some(OrbCell::Fill { symbol, color_band })
    } else if distance < radius + 1.25 {
        Some(OrbCell::Halo)
    } else {
        None
    }
}

fn orb_layout_available(area: Rect) -> bool {
    area.width >= ORB_PROMPT_WIDTH.saturating_add(4)
        && area.height >= ORB_PROMPT_HEIGHT.saturating_add(2)
}

fn prompt_size(area: Rect) -> (u16, u16) {
    if orb_layout_available(area) {
        (ORB_PROMPT_WIDTH, ORB_PROMPT_HEIGHT)
    } else {
        (PROMPT_WIDTH, PROMPT_HEIGHT)
    }
}

fn prompt_inner_rect(area: Rect) -> Option<Rect> {
    let (width, height) = prompt_size(area);
    centered_popup_rect(area, width, height).map(|popup| {
        Rect::new(
            popup.x.saturating_add(1),
            popup.y.saturating_add(1),
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        )
    })
}

fn prompt_content_rect(inner: Rect, orb_visible: bool) -> Rect {
    if orb_visible {
        Rect::new(
            inner.x.saturating_add(ORB_COLUMN_WIDTH + 1),
            inner.y,
            ORB_CONTENT_WIDTH.min(inner.width.saturating_sub(ORB_COLUMN_WIDTH + 1)),
            inner.height,
        )
    } else {
        inner
    }
}

fn send_off_rect(area: Rect) -> Option<Rect> {
    centered_popup_rect(area, SEND_OFF_WIDTH, SEND_OFF_HEIGHT)
        .filter(|popup| popup.width == SEND_OFF_WIDTH && popup.height == SEND_OFF_HEIGHT)
}

pub(crate) fn input_presentation_at(
    app: &AppState,
    area: Rect,
    now: std::time::Instant,
) -> InputPresentation {
    let prompt = app.pomodoro.prompt.as_ref().and_then(|_| {
        let (width, height) = prompt_size(area);
        let popup = centered_popup_rect(area, width, height)?;
        let inner = Rect::new(
            popup.x.saturating_add(1),
            popup.y.saturating_add(1),
            popup.width.saturating_sub(2),
            popup.height.saturating_sub(2),
        );
        (inner.height >= 9).then_some(popup)
    });
    let send_off = app.pomodoro.send_off.as_ref().and_then(|send_off| {
        (now < send_off.shown_at + SEND_OFF_DURATION)
            .then(|| send_off_rect(area))
            .flatten()
    });
    InputPresentation {
        area,
        send_off,
        prompt,
    }
}

/// Adds overlays raised since the last committed frame without clearing any
/// ownership that frame established. Only a later frame commit may narrow it.
pub(crate) fn input_gate_at(
    app: &AppState,
    gate: InputPresentation,
    now: std::time::Instant,
) -> InputPresentation {
    let current = input_presentation_at(app, gate.area, now);
    InputPresentation {
        area: gate.area,
        send_off: gate.send_off.or(current.send_off),
        prompt: gate.prompt.or(current.prompt),
    }
}

pub(crate) fn send_off_visible_at(app: &AppState, area: Rect, now: std::time::Instant) -> bool {
    input_presentation_at(app, area, now).send_off.is_some()
}

pub(crate) fn animation_visible_at(app: &AppState, area: Rect, now: std::time::Instant) -> bool {
    if app.pomodoro.prompt.is_some() {
        return orb_layout_available(area)
            && prompt_inner_rect(area).is_some_and(|inner| inner.height >= 9);
    }
    send_off_visible_at(app, area, now)
}

pub(crate) fn animation_deadline_at(
    app: &AppState,
    area: Rect,
    now: std::time::Instant,
) -> Option<std::time::Instant> {
    let started_at = animation_started_at(app)?;
    if app.pomodoro.prompt.is_some() {
        if !animation_visible_at(app, area, now) {
            return None;
        }
        return Some(next_animation_frame_at(now, started_at));
    }
    let send_off = app.pomodoro.send_off.as_ref()?;
    if !send_off_visible_at(app, area, now) {
        return None;
    }
    Some(next_animation_frame_at(now, started_at).min(send_off.shown_at + SEND_OFF_DURATION))
}

pub(crate) fn animation_due_at(
    app: &AppState,
    area: Rect,
    now: std::time::Instant,
    last_rendered_at: Option<std::time::Instant>,
) -> bool {
    if !animation_visible_at(app, area, now) {
        return false;
    }
    let Some(started_at) = animation_started_at(app) else {
        return false;
    };
    let Some(last_rendered_at) = last_rendered_at else {
        return true;
    };
    animation_frame_at(now, started_at) > animation_frame_at(last_rendered_at, started_at)
}

pub(crate) fn prompt_button_rects(area: Rect) -> Option<(Rect, Rect)> {
    let inner = prompt_inner_rect(area)?;
    if inner.height < 9 {
        return None;
    }
    let orb_visible = orb_layout_available(area);
    let content = prompt_content_rect(inner, orb_visible);
    let buttons = [
        ActionButtonSpec {
            hint: Some("↵"),
            label: "confirm",
        },
        ActionButtonSpec {
            hint: Some("^⌥b"),
            label: "snooze",
        },
    ];
    let needed_width = buttons
        .iter()
        .map(|button| super::widgets::action_button_width(button.hint, button.label))
        .sum::<u16>()
        .saturating_add(2);
    let rects = if needed_width <= content.width {
        action_button_row_rects(content, &buttons, 2, content.height.saturating_sub(1))
    } else {
        let first_row = content.height.saturating_sub(2);
        buttons
            .iter()
            .enumerate()
            .flat_map(|(index, button)| {
                action_button_row_rects(
                    content,
                    std::slice::from_ref(button),
                    0,
                    first_row.saturating_add(index as u16),
                )
            })
            .collect()
    };
    Some((*rects.first()?, *rects.get(1)?))
}

/// The countdown's slot at the right end of the sidebar footer row. Empty when
/// the timer is off or the sidebar is too narrow to hold it beside the icons.
pub(crate) fn pomodoro_hit_area(app: &AppState, sidebar: Rect) -> Rect {
    if !app.pomodoro.enabled
        || !app.pomodoro.sidebar_visible
        || (app.sidebar_sections_layout && !app.sidebar_areas.pomodoro)
        || app.sidebar_collapsed
        || sidebar.height == 0
        || (app.sidebar_sections_layout
            && sidebar.height < super::sidebar::sidebar_workspace_floor(app).saturating_add(3))
    {
        return Rect::default();
    }
    let content_width = sidebar.width.saturating_sub(1);
    let area_width = if app.sidebar_sections_layout {
        SIDEBAR_AREAS_WIDTH
    } else {
        0
    };
    if content_width
        < FOOTER_ICON_COLUMNS + CYCLE_MODE_WIDTH + NOTIFICATION_WIDTH + area_width + INDICATOR_WIDTH
    {
        return Rect::default();
    }
    Rect::new(
        sidebar.x + content_width - INDICATOR_WIDTH,
        sidebar.bottom().saturating_sub(1),
        INDICATOR_WIDTH,
        1,
    )
}

/// The per-machine notification toggle beside the timer. It keeps the
/// right-most footer slot when the timer is hidden so it remains available at
/// the supported minimum sidebar width.
pub(crate) fn notification_hit_area(app: &AppState, sidebar: Rect) -> Rect {
    if app.sidebar_collapsed || sidebar.height == 0 {
        return Rect::default();
    }
    let content_width = sidebar.width.saturating_sub(1);
    if app.sidebar_sections_layout {
        let x = sidebar
            .x
            .saturating_add(FOOTER_ICON_COLUMNS + CYCLE_MODE_WIDTH + SIDEBAR_AREAS_WIDTH + 1);
        let timer = pomodoro_hit_area(app, sidebar);
        let right_limit = sidebar.x.saturating_add(content_width).saturating_sub(2);
        if x.saturating_add(NOTIFICATION_WIDTH) > right_limit
            || (timer.width > 0 && x.saturating_add(NOTIFICATION_WIDTH) > timer.x)
        {
            return Rect::default();
        }
        return Rect::new(x, sidebar.bottom().saturating_sub(1), NOTIFICATION_WIDTH, 1);
    }
    if content_width < FOOTER_ICON_COLUMNS + CYCLE_MODE_WIDTH + NOTIFICATION_WIDTH {
        return Rect::default();
    }
    let timer = pomodoro_hit_area(app, sidebar);
    let x = if timer.width > 0 {
        timer.x.saturating_sub(NOTIFICATION_WIDTH)
    } else {
        sidebar.x + content_width - NOTIFICATION_WIDTH
    };
    Rect::new(x, sidebar.bottom().saturating_sub(1), NOTIFICATION_WIDTH, 1)
}

pub(crate) fn window_cycle_mode_hit_area(app: &AppState, sidebar: Rect) -> Rect {
    if app.sidebar_sections_layout {
        if app.sidebar_collapsed || sidebar.height == 0 || sidebar.width.saturating_sub(1) < 21 {
            return Rect::default();
        }
        let slot = Rect::new(
            sidebar.x.saturating_add(FOOTER_ICON_COLUMNS),
            sidebar.bottom().saturating_sub(1),
            CYCLE_MODE_WIDTH,
            1,
        );
        let timer = pomodoro_hit_area(app, sidebar);
        return if timer.width > 0 && slot.right() > timer.x {
            Rect::default()
        } else {
            slot
        };
    }
    let notification = notification_hit_area(app, sidebar);
    if notification.width == 0 {
        return Rect::default();
    }
    // The cycle icon trails the one-cell focus board icon.
    Rect::new(
        sidebar.x + FOOTER_ICON_COLUMNS,
        notification.y,
        CYCLE_MODE_WIDTH,
        notification.height,
    )
}

pub(crate) fn sidebar_areas_hit_area(app: &AppState, sidebar: Rect) -> Rect {
    if !app.sidebar_sections_layout || app.sidebar_collapsed || sidebar.height == 0 {
        return Rect::default();
    }
    let content_width = sidebar.width.saturating_sub(1);
    let x = sidebar
        .x
        .saturating_add(FOOTER_ICON_COLUMNS + CYCLE_MODE_WIDTH + 1);
    let right_limit = sidebar.x.saturating_add(content_width).saturating_sub(2);
    let timer = pomodoro_hit_area(app, sidebar);
    if content_width < FOOTER_ICON_COLUMNS + CYCLE_MODE_WIDTH + SIDEBAR_AREAS_WIDTH + 3
        || x.saturating_add(SIDEBAR_AREAS_WIDTH) > right_limit
        || (timer.width > 0 && x.saturating_add(SIDEBAR_AREAS_WIDTH) > timer.x)
    {
        return Rect::default();
    }
    Rect::new(
        x,
        sidebar.bottom().saturating_sub(1),
        SIDEBAR_AREAS_WIDTH,
        1,
    )
}

pub(crate) fn render_window_cycle_mode_toggle(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let selected = app.window_cycle_menu_open
        || app.window_cycle_mode != crate::config::WindowCycleModeConfig::ThisMachine
        || app.skip_collapsed_cycle;
    let hovered = app.hovered_control
        == Some(crate::app::state::ControlId::SidebarFooter(
            crate::app::state::SidebarFooterItem::WindowCycleMode,
        ));
    let style = if selected {
        Style::default()
            .fg(app.palette.accent)
            .bg(app.palette.surface0)
    } else if hovered {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
    } else {
        Style::default().fg(app.palette.overlay0)
    };
    let glyph = crate::ui::icons::cycle_label(app.nerd_font);
    frame.render_widget(Paragraph::new(Span::styled(glyph, style)), area);
}

fn phase_color(app: &AppState, phase: PomodoroPhase) -> ratatui::style::Color {
    if phase.is_break() {
        app.palette.green
    } else {
        app.palette.text
    }
}

/// Draws the countdown. `now` comes from the frame's observation instant so the
/// label matches the rest of the frame.
pub(crate) fn render_indicator(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    now: std::time::Instant,
) {
    if area.width == 0 || area.height == 0 || !app.pomodoro.enabled || !app.pomodoro.sidebar_visible
    {
        return;
    }
    let held = app.pomodoro.held();
    let paused = app.pomodoro.paused();
    let glyph = if paused { "‖" } else { "⏱" };
    let style = if held {
        Style::default().fg(app.palette.yellow)
    } else if paused {
        Style::default().fg(app.palette.overlay0)
    } else {
        Style::default().fg(phase_color(app, app.pomodoro.phase))
    };
    let label = if held {
        format!("{glyph} held")
    } else {
        format!("{glyph} {}", app.pomodoro.label_at(now))
    };
    let line = Line::from(vec![Span::styled(label, style)]);
    frame.render_widget(Paragraph::new(line).right_aligned(), area);
}

pub(crate) fn render_notification_toggle(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let (glyph, color) = if app.notifications_enabled() {
        ("🔔", app.palette.text)
    } else {
        ("🔕", app.palette.overlay0)
    };
    frame.render_widget(
        Paragraph::new(Span::styled(glyph, Style::default().fg(color))).right_aligned(),
        area,
    );
}

pub(crate) fn render_sidebar_areas_toggle(app: &AppState, frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let color = if app.sidebar_areas_menu_selected.is_some() {
        app.palette.accent
    } else {
        app.palette.overlay0
    };
    frame.render_widget(
        Paragraph::new(Span::styled("≡", Style::default().fg(color))),
        area,
    );
}

/// The due prompt or its short post-confirm acknowledgment.
pub(crate) fn render_overlay(app: &AppState, frame: &mut Frame, area: Rect) {
    if let Some(prompt) = app.pomodoro.prompt.as_ref() {
        render_prompt(app, frame, area, prompt);
        return;
    }
    render_send_off(app, frame, area);
}

fn render_prompt(app: &AppState, frame: &mut Frame, area: Rect, prompt: &PomodoroPrompt) {
    let Some(_) = prompt_inner_rect(area).filter(|inner| inner.height >= 9) else {
        return;
    };
    let palette = &app.palette;
    super::veil_background(frame, area, palette);
    let (width, height) = prompt_size(area);
    let Some(inner) = render_modal_shell(frame, area, width, height, palette) else {
        return;
    };
    if orb_layout_available(area) {
        render_orb_prompt(app, frame, area, inner, prompt);
    } else {
        render_compact_prompt(app, frame, area, inner, prompt);
    }
}

fn render_compact_prompt(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    prompt: &PomodoroPrompt,
) {
    let palette = &app.palette;
    let rows = Layout::vertical([
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(2),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Length(1),
        Constraint::Min(0),
    ])
    .areas::<9>(inner);

    let title = prompt_title(prompt, inner.width);
    render_modal_header(frame, rows[0], title, palette);

    let body = prompt_body(app, prompt, inner.width);
    frame.render_widget(
        Paragraph::new(Span::styled(body, Style::default().fg(palette.text)))
            .wrap(Wrap { trim: true }),
        rows[2],
    );

    let hint = if inner.width < 32 {
        format!("min {} chars", app.pomodoro.min_confirm_chars)
    } else {
        format!(
            "type at least {} characters to confirm:",
            app.pomodoro.min_confirm_chars
        )
    };
    frame.render_widget(
        Paragraph::new(Span::styled(hint, Style::default().fg(palette.overlay0))),
        rows[3],
    );

    render_input(app, frame, rows[4], &prompt.input);

    if let Some(error) = prompt.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Span::styled(
                error.to_string(),
                Style::default().fg(palette.red),
            )),
            rows[5],
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "^c clear".to_string(),
            Style::default().fg(palette.overlay0),
        )),
        rows[6],
    );

    render_prompt_buttons(app, frame, area);
}

fn render_orb_prompt(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    inner: Rect,
    prompt: &PomodoroPrompt,
) {
    let palette = &app.palette;
    let content = prompt_content_rect(inner, true);
    render_modal_header(
        frame,
        Rect::new(content.x, content.y + 1, content.width, 1),
        prompt_title(prompt, content.width),
        palette,
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "take a deep breath.",
            Style::default()
                .fg(palette.text)
                .add_modifier(Modifier::BOLD),
        )),
        Rect::new(content.x, content.y + 3, content.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            prompt_body(app, prompt, content.width),
            Style::default().fg(palette.text),
        ))
        .wrap(Wrap { trim: true }),
        Rect::new(content.x, content.y + 5, content.width, 2),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!(
                "type at least {} characters to confirm:",
                app.pomodoro.min_confirm_chars
            ),
            Style::default().fg(palette.overlay0),
        )),
        Rect::new(content.x, content.y + 8, content.width, 1),
    );
    render_input(
        app,
        frame,
        Rect::new(content.x, content.y + 9, content.width, 1),
        &prompt.input,
    );
    if let Some(error) = prompt.error.as_deref() {
        frame.render_widget(
            Paragraph::new(Span::styled(error, Style::default().fg(palette.red))),
            Rect::new(content.x, content.y + 10, content.width, 1),
        );
    }
    frame.render_widget(
        Paragraph::new(Span::styled(
            "^c clear",
            Style::default().fg(palette.overlay0),
        )),
        Rect::new(content.x, content.y + 11, content.width, 1),
    );

    let elapsed = animation_elapsed_at(app.view_observed_at, prompt.raised_at);
    render_orb(app, frame, inner, breath_at(elapsed));
    let tip = prompt_tip(prompt.ended, elapsed);
    frame.render_widget(
        Paragraph::new(Span::styled(tip, Style::default().fg(palette.subtext0))),
        Rect::new(content.x, content.y + 17, content.width, 1),
    );
    render_prompt_buttons(app, frame, area);
}

fn render_orb(app: &AppState, frame: &mut Frame, inner: Rect, breath: BreathFrame) {
    let palette = &app.palette;
    let radius = ORB_MIN_RADIUS + (ORB_MAX_RADIUS - ORB_MIN_RADIUS) * breath.progress;
    let center_x = inner.x + ORB_COLUMN_WIDTH / 2;
    let center_y = inner.y + 8;
    for row in 1..=15 {
        for column in 1..ORB_COLUMN_WIDTH.saturating_sub(1) {
            let x = inner.x + column;
            let y = inner.y + row;
            let Some(orb) = orb_cell(
                i32::from(x) - i32::from(center_x),
                i32::from(y) - i32::from(center_y),
                radius,
            ) else {
                continue;
            };
            let cell = &mut frame.buffer_mut()[(x, y)];
            match orb {
                OrbCell::Fill { symbol, color_band } => {
                    let color = match color_band {
                        0 => palette.teal,
                        1 => palette.blue,
                        _ => palette.mauve,
                    };
                    cell.set_symbol(symbol)
                        .set_fg(color)
                        .set_bg(palette.panel_bg);
                    if symbol == "█" {
                        cell.set_style(cell.style().add_modifier(Modifier::BOLD));
                    }
                }
                OrbCell::Halo if (x + y).is_multiple_of(3) => {
                    cell.set_symbol("·")
                        .set_fg(palette.surface1)
                        .set_bg(palette.panel_bg);
                }
                OrbCell::Halo => {}
            }
        }
    }
    let cue_color = if breath.stage == BreathStage::Hold {
        palette.mauve
    } else {
        palette.teal
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            format!("{} · {}", breath.cue, breath.remaining),
            Style::default().fg(cue_color).add_modifier(Modifier::BOLD),
        ))
        .centered(),
        Rect::new(inner.x, inner.y + 16, ORB_COLUMN_WIDTH, 1),
    );
}

fn prompt_title(prompt: &PomodoroPrompt, width: u16) -> &'static str {
    if prompt.ended.is_break() {
        "break over"
    } else if width < 16 {
        "break due"
    } else {
        "time for a break"
    }
}

fn prompt_body(app: &AppState, prompt: &PomodoroPrompt, width: u16) -> String {
    let minutes = app.pomodoro.phase_duration(prompt.next).as_secs() / 60;
    if width < 32 {
        format!(
            "{} → {} ({minutes}m)",
            prompt.ended.label(),
            prompt.next.label()
        )
    } else {
        format!(
            "{} finished. next up: {} for {minutes} minutes.",
            prompt.ended.label(),
            prompt.next.label()
        )
    }
}

fn prompt_tip(ended: PomodoroPhase, elapsed: std::time::Duration) -> &'static str {
    if ended.is_break() {
        "back to it. one thing at a time."
    } else {
        BREAK_TIPS[(elapsed.as_secs() / 4) as usize % BREAK_TIPS.len()]
    }
}

fn render_prompt_buttons(app: &AppState, frame: &mut Frame, area: Rect) {
    let palette = &app.palette;
    let Some((confirm, snooze)) = prompt_button_rects(area) else {
        return;
    };
    render_action_button(
        frame,
        confirm,
        Some("↵"),
        "confirm",
        Style::default()
            .fg(panel_contrast_fg(palette))
            .bg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    render_action_button(
        frame,
        snooze,
        Some("^⌥b"),
        "snooze",
        Style::default()
            .fg(palette.text)
            .bg(palette.surface0)
            .add_modifier(Modifier::BOLD),
    );
}

fn render_send_off(app: &AppState, frame: &mut Frame, area: Rect) {
    let Some(send_off) = app
        .pomodoro
        .send_off
        .as_ref()
        .filter(|send_off| app.view_observed_at < send_off.shown_at + SEND_OFF_DURATION)
    else {
        return;
    };
    let palette = &app.palette;
    let border = if send_off.started.is_break() {
        palette.green
    } else {
        palette.mauve
    };
    let Some(popup) = send_off_rect(area) else {
        return;
    };
    let Some(inner) = render_panel_shell(frame, popup, border, palette.panel_bg) else {
        return;
    };
    if inner.height < 5 {
        return;
    }
    let title = if send_off.started.is_break() {
        "enjoy the break"
    } else {
        "welcome back"
    };
    let title_color = if send_off.started.is_break() {
        palette.green
    } else {
        palette.mauve
    };
    frame.render_widget(
        Paragraph::new(Span::styled(
            title,
            Style::default()
                .fg(title_color)
                .add_modifier(Modifier::BOLD),
        ))
        .centered(),
        Rect::new(inner.x, inner.y, inner.width, 1),
    );
    let tip = if send_off.started.is_break() {
        let index =
            app.pomodoro.completed_work_intervals.saturating_sub(1) as usize % BREAK_TIPS.len();
        BREAK_TIPS[index]
    } else {
        "one thing at a time."
    };
    frame.render_widget(
        Paragraph::new(Span::styled(tip, Style::default().fg(palette.subtext0))).centered(),
        Rect::new(inner.x, inner.y + 1, inner.width, 1),
    );
    let elapsed = animation_elapsed_at(app.view_observed_at, send_off.shown_at);
    frame.render_widget(
        Paragraph::new(Span::styled(
            send_off_dots(breath_at(elapsed).progress),
            Style::default().fg(palette.teal),
        ))
        .centered(),
        Rect::new(inner.x, inner.y + 3, inner.width, 1),
    );
    frame.render_widget(
        Paragraph::new(Span::styled(
            "any key closes",
            Style::default().fg(palette.overlay0),
        ))
        .right_aligned(),
        Rect::new(inner.x, inner.y + 4, inner.width, 1),
    );
}

fn send_off_dots(progress: f64) -> String {
    let lit = (progress * 4.0).round() as i8;
    (0_i8..9)
        .map(|index| {
            let distance = (index - 4).abs();
            if distance < lit {
                '●'
            } else if distance == lit {
                '•'
            } else {
                '·'
            }
        })
        .fold(String::new(), |mut dots, symbol| {
            if !dots.is_empty() {
                dots.push(' ');
            }
            dots.push(symbol);
            dots
        })
}

fn render_input(app: &AppState, frame: &mut Frame, area: Rect, input: &str) {
    if area.width == 0 {
        return;
    }
    frame.render_widget(Clear, area);
    let text_rect = Rect {
        width: area.width.saturating_sub(1),
        ..area
    };
    frame.render_widget(
        Paragraph::new(format!(" {input}")).style(
            Style::default()
                .fg(app.palette.text)
                .bg(app.palette.surface0),
        ),
        text_rect,
    );
    let caret = area
        .x
        .saturating_add(1)
        .saturating_add(display_width_u16(input))
        .min(area.right().saturating_sub(1));
    frame.set_cursor_position((caret, area.y));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::AppState;

    fn state() -> AppState {
        let mut app = AppState::test_new();
        app.pomodoro.enabled = true;
        app.pomodoro.sidebar_visible = true;
        app
    }

    #[test]
    fn breath_cycle_uses_four_one_five_timing_and_easing() {
        let start = breath_at(std::time::Duration::ZERO);
        assert_eq!(start.stage, BreathStage::Inhale);
        assert_eq!(start.remaining, 4);
        assert_eq!(start.progress, 0.0);

        let inhale = breath_at(std::time::Duration::from_secs(2));
        assert_eq!(inhale.stage, BreathStage::Inhale);
        assert_eq!(inhale.remaining, 2);
        assert!((inhale.progress - 0.5).abs() < f64::EPSILON);

        let hold = breath_at(std::time::Duration::from_millis(4_500));
        assert_eq!(hold.stage, BreathStage::Hold);
        assert_eq!(hold.remaining, 1);
        assert_eq!(hold.progress, 1.0);

        let exhale = breath_at(std::time::Duration::from_millis(7_500));
        assert_eq!(exhale.stage, BreathStage::Exhale);
        assert_eq!(exhale.remaining, 3);
        assert!((exhale.progress - 0.5).abs() < f64::EPSILON);
        let early_exhale = breath_at(std::time::Duration::from_secs(6));
        assert_eq!(early_exhale.cue, "breathe out");
        assert_eq!(early_exhale.remaining, 4);
        assert_eq!(breath_at(std::time::Duration::from_secs(10)), start);
    }

    #[test]
    fn visual_time_is_quantized_to_the_animation_rate() {
        let started_at = std::time::Instant::now();
        assert_eq!(
            animation_elapsed_at(
                started_at + ANIMATION_FRAME_INTERVAL - std::time::Duration::from_millis(1),
                started_at
            ),
            std::time::Duration::ZERO
        );
        assert_eq!(
            animation_elapsed_at(started_at + ANIMATION_FRAME_INTERVAL, started_at),
            ANIMATION_FRAME_INTERVAL
        );
    }

    #[test]
    fn animation_wakes_only_for_a_visible_card_at_eight_fps_or_less() {
        let started_at = std::time::Instant::now();
        let mut app = state();
        app.pomodoro.prompt = Some(PomodoroPrompt {
            ended: PomodoroPhase::Work,
            next: PomodoroPhase::ShortBreak,
            raised_at: started_at,
            input: String::new(),
            error: None,
        });
        let wide = Rect::new(0, 0, 100, 30);
        let compact = Rect::new(0, 0, 18, 30);

        assert_eq!(
            animation_deadline_at(&app, wide, started_at),
            Some(started_at + ANIMATION_FRAME_INTERVAL)
        );
        assert!(!animation_due_at(
            &app,
            wide,
            started_at + ANIMATION_FRAME_INTERVAL - std::time::Duration::from_millis(1),
            Some(started_at),
        ));
        assert!(animation_due_at(
            &app,
            wide,
            started_at + ANIMATION_FRAME_INTERVAL,
            Some(started_at),
        ));
        assert_eq!(animation_deadline_at(&app, compact, started_at), None);
        assert!(!animation_due_at(
            &app,
            compact,
            started_at,
            Some(started_at)
        ));
    }

    #[test]
    fn orb_geometry_is_elliptical_and_uses_all_density_bands() {
        assert_eq!(
            orb_cell(0, 0, ORB_MAX_RADIUS),
            Some(OrbCell::Fill {
                symbol: "█",
                color_band: 0,
            })
        );
        assert!(matches!(
            orb_cell(5, 0, ORB_MAX_RADIUS),
            Some(OrbCell::Fill { symbol: "▓", .. })
        ));
        assert!(matches!(
            orb_cell(8, 0, ORB_MAX_RADIUS),
            Some(OrbCell::Fill { symbol: "▒", .. })
        ));
        assert!(matches!(
            orb_cell(12, 0, ORB_MAX_RADIUS),
            Some(OrbCell::Fill { symbol: "░", .. })
        ));
        assert!(matches!(
            orb_cell(17, 0, ORB_MAX_RADIUS),
            Some(OrbCell::Halo)
        ));
        assert_eq!(orb_cell(18, 0, ORB_MAX_RADIUS), None);
        assert!(orb_cell(16, 0, ORB_MAX_RADIUS).is_some());
        assert!(orb_cell(0, 9, ORB_MAX_RADIUS).is_none());
    }

    #[test]
    fn send_off_dots_pulse_from_the_center() {
        assert_eq!(send_off_dots(0.0), "· · · · • · · · ·");
        assert_eq!(send_off_dots(1.0), "• ● ● ● ● ● ● ● •");
    }

    #[test]
    fn prompt_tips_rotate_for_break_start_and_settle_for_break_over() {
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::ZERO),
            BREAK_TIPS[0]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::from_secs(4)),
            BREAK_TIPS[1]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::Work, std::time::Duration::from_secs(8)),
            BREAK_TIPS[2]
        );
        assert_eq!(
            prompt_tip(PomodoroPhase::ShortBreak, std::time::Duration::from_secs(8)),
            "back to it. one thing at a time."
        );
    }

    #[test]
    fn a_disabled_timer_claims_no_footer_slot() {
        let mut app = state();
        app.pomodoro.enabled = false;
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 30, 20)),
            Rect::default()
        );
    }

    #[test]
    fn a_hidden_widget_claims_no_footer_slot_while_the_timer_runs() {
        let mut app = state();
        app.pomodoro.sidebar_visible = false;
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 30, 20)),
            Rect::default()
        );
    }

    #[test]
    fn the_indicator_is_dropped_before_it_overlaps_the_footer_icons() {
        let app = state();
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 29, 20)),
            Rect::default()
        );
        assert_eq!(
            pomodoro_hit_area(&app, Rect::new(0, 0, 30, 20)),
            Rect::new(21, 19, 8, 1)
        );
    }

    #[test]
    fn fleet_workspace_ac6_cycle_icon_trails_left_footer_icons() {
        let app = state();
        let narrow = Rect::new(0, 0, 22, 20);
        assert_eq!(pomodoro_hit_area(&app, narrow), Rect::default());
        assert_eq!(notification_hit_area(&app, narrow), Rect::new(19, 19, 2, 1));
        assert_eq!(
            window_cycle_mode_hit_area(&app, narrow),
            Rect::new(18, 19, 1, 1)
        );

        let almost_wide = Rect::new(0, 0, 28, 20);
        assert_eq!(pomodoro_hit_area(&app, almost_wide), Rect::default());
        assert_eq!(
            notification_hit_area(&app, almost_wide),
            Rect::new(25, 19, 2, 1)
        );
        assert_eq!(
            window_cycle_mode_hit_area(&app, almost_wide),
            Rect::new(18, 19, 1, 1)
        );

        // The one-column cycle icon leaves the countdown room at 30 columns.
        let default_width = Rect::new(0, 0, 30, 20);
        assert_eq!(
            pomodoro_hit_area(&app, default_width),
            Rect::new(21, 19, 8, 1)
        );

        let wide = Rect::new(0, 0, 31, 20);
        let bell = notification_hit_area(&app, wide);
        let timer = pomodoro_hit_area(&app, wide);
        assert_eq!(bell, Rect::new(20, 19, 2, 1));
        assert_eq!(
            window_cycle_mode_hit_area(&app, wide),
            Rect::new(18, 19, 1, 1)
        );
        assert_eq!(timer, Rect::new(22, 19, 8, 1));
        assert_eq!(bell.right(), timer.x);
    }

    #[test]
    fn sections_footer_controls_render_in_left_slots_with_matching_hit_areas() {
        let mut app = state();
        app.sidebar_sections_layout = true;
        let sidebar = Rect::new(0, 0, 40, 20);
        let areas = sidebar_areas_hit_area(&app, sidebar);
        let bell = notification_hit_area(&app, sidebar);
        let cycle = window_cycle_mode_hit_area(&app, sidebar);
        assert!(areas.x < bell.x);
        assert!(bell.right() <= sidebar.x + sidebar.width - 3);
        assert!(areas.x > cycle.right());
        assert!(bell.right() <= pomodoro_hit_area(&app, sidebar).x);

        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(40, 20))
            .expect("footer terminal");
        terminal
            .draw(|frame| {
                render_sidebar_areas_toggle(&app, frame, areas);
                render_notification_toggle(&app, frame, bell);
            })
            .expect("draw footer controls");
        assert_eq!(
            terminal.backend().buffer()[(areas.x, areas.y)].symbol(),
            "≡"
        );
        assert_eq!(terminal.backend().buffer()[(bell.x, bell.y)].symbol(), "🔕");

        let narrow = Rect::new(0, 0, 18, 20);
        assert_eq!(sidebar_areas_hit_area(&app, narrow).width, 0);
        assert_eq!(notification_hit_area(&app, narrow).width, 0);
    }

    #[test]
    fn sections_footer_hit_areas_never_overlap_at_supported_widths() {
        let mut app = state();
        app.sidebar_sections_layout = true;
        for width in 18..=60 {
            let sidebar = Rect::new(0, 0, width, 20);
            let areas: Vec<Rect> = [
                sidebar_areas_hit_area(&app, sidebar),
                window_cycle_mode_hit_area(&app, sidebar),
                notification_hit_area(&app, sidebar),
                pomodoro_hit_area(&app, sidebar),
            ]
            .into_iter()
            .filter(|area| area.width > 0)
            .collect();
            for (index, left) in areas.iter().enumerate() {
                if *left != pomodoro_hit_area(&app, sidebar) {
                    assert!(left.right() <= width - 2, "width {width}: {left:?}");
                }
                for right in &areas[index + 1..] {
                    assert!(
                        left.right() <= right.x || right.right() <= left.x,
                        "width {width}: overlapping {left:?} and {right:?}"
                    );
                }
            }
        }
        app.pomodoro.enabled = false;
        for width in 18..=60 {
            let sidebar = Rect::new(0, 0, width, 20);
            let areas: Vec<Rect> = [
                sidebar_areas_hit_area(&app, sidebar),
                window_cycle_mode_hit_area(&app, sidebar),
                notification_hit_area(&app, sidebar),
                pomodoro_hit_area(&app, sidebar),
            ]
            .into_iter()
            .filter(|area| area.width > 0)
            .collect();
            for (index, left) in areas.iter().enumerate() {
                for right in &areas[index + 1..] {
                    assert!(
                        left.right() <= right.x || right.right() <= left.x,
                        "disabled timer width {width}: overlapping {left:?} and {right:?}"
                    );
                }
            }
        }
    }

    #[test]
    fn notification_bell_renders_on_and_muted_states_without_the_timer() {
        let mut app = state();
        app.pomodoro.enabled = false;
        let area = Rect::new(0, 0, 22, 1);
        let slot = notification_hit_area(&app, area);
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(22, 1))
            .expect("test terminal");

        terminal
            .draw(|frame| render_notification_toggle(&app, frame, slot))
            .expect("draw muted bell");
        let muted = &terminal.backend().buffer()[(slot.x, slot.y)];
        assert_eq!(muted.symbol(), "🔕");
        assert_eq!(muted.style().fg, Some(app.palette.overlay0));

        app.toast_config.delivery = crate::config::ToastDelivery::Terminal;
        app.sound.enabled = true;
        terminal
            .draw(|frame| render_notification_toggle(&app, frame, slot))
            .expect("draw active bell");
        let active = &terminal.backend().buffer()[(slot.x, slot.y)];
        assert_eq!(active.symbol(), "🔔");
        assert_eq!(active.style().fg, Some(app.palette.text));
    }

    #[test]
    fn f1_held_indicator_names_the_state_in_a_distinct_colour() {
        let now = std::time::Instant::now();
        let mut app = state();
        app.pomodoro.reset(now);
        app.pomodoro
            .tick_with_host_focus(now + std::time::Duration::from_secs(25 * 60), false);
        let sidebar = Rect::new(0, 0, 30, 1);
        let slot = pomodoro_hit_area(&app, sidebar);
        assert_ne!(slot, Rect::default());
        let mut terminal = ratatui::Terminal::new(ratatui::backend::TestBackend::new(30, 1))
            .expect("test terminal");
        terminal
            .draw(|frame| render_indicator(&app, frame, slot, now))
            .expect("draw indicator");
        let buffer = terminal.backend().buffer();
        let text: String = (slot.x..slot.right())
            .map(|x| buffer[(x, slot.y)].symbol())
            .collect();
        let held_cell = (slot.x..slot.right())
            .map(|x| &buffer[(x, slot.y)])
            .find(|cell| cell.symbol() == "h")
            .expect("held label");

        assert!(text.contains("⏱ held"), "{text:?}");
        assert_eq!(held_cell.style().fg, Some(app.palette.yellow));
        assert_ne!(app.palette.yellow, phase_color(&app, app.pomodoro.phase));
    }
}

#[cfg(test)]
mod render_tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};

    fn prompted() -> AppState {
        use crate::pomodoro::{PomodoroPhase, PomodoroPrompt};
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro.enabled = true;
        app.pomodoro.prompt = Some(PomodoroPrompt {
            ended: PomodoroPhase::Work,
            next: PomodoroPhase::ShortBreak,
            raised_at: now,
            input: String::new(),
            error: None,
        });
        app.view_observed_at = now;
        app
    }

    /// Draws work first, like the panes are, then the prompt over it.
    fn veiled_frame(app: &AppState, area: Rect) -> ratatui::buffer::Buffer {
        let mut terminal =
            Terminal::new(TestBackend::new(area.width, area.height)).expect("test terminal");
        terminal
            .draw(|frame| {
                frame.render_widget(
                    Paragraph::new("cargo test --all"),
                    Rect::new(0, 0, area.width, 1),
                );
                render_overlay(app, frame, area);
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    /// A break reminder is only blocking if the work behind it stops competing
    /// for attention. The DIM modifier alone is a no-op on several terminals.
    #[test]
    fn the_confirm_overlay_veils_the_work_behind_it() {
        let app = prompted();
        let area = Rect::new(0, 0, 80, 24);
        let buffer = veiled_frame(&app, area);

        let veiled = &buffer[(0, 0)];
        assert_eq!(veiled.symbol(), " ", "work behind the prompt is blanked");
        assert_eq!(veiled.style().bg, Some(app.palette.surface0));

        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(
            !text.contains("cargo test"),
            "veil hides the work: {text:?}"
        );
        assert!(
            text.contains("time for a break"),
            "modal draws above the veil"
        );
    }

    /// `surface0` follows the terminal's own background in the 16-colour theme,
    /// so using it there would leave no veil at all.
    #[test]
    fn the_veil_falls_back_to_a_concrete_colour_on_the_terminal_theme() {
        let mut app = prompted();
        app.palette = crate::app::state::Palette::terminal();
        assert_eq!(app.palette.surface0, ratatui::style::Color::Reset);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 80, 24));
        assert_eq!(buffer[(0, 0)].style().bg, Some(app.palette.surface1));
    }

    #[test]
    fn wide_prompt_draws_the_breathing_orb_cue_copy_and_tip() {
        let mut app = prompted();
        let raised_at = app.pomodoro.prompt.as_ref().expect("prompt").raised_at;
        app.view_observed_at = raised_at + std::time::Duration::from_secs(1);
        app.palette.accent = ratatui::style::Color::Rgb(10, 20, 30);
        app.palette.blue = ratatui::style::Color::Rgb(40, 50, 60);
        app.palette.teal = ratatui::style::Color::Rgb(70, 80, 90);
        app.palette.mauve = ratatui::style::Color::Rgb(100, 110, 120);

        let area = Rect::new(0, 0, 100, 30);
        let inner = prompt_inner_rect(area).expect("orb prompt geometry");
        assert_eq!(inner.width, ORB_PROMPT_WIDTH - 2);
        assert_eq!(inner.height, ORB_PROMPT_HEIGHT - 2);
        let buffer = veiled_frame(&app, area);
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(text.contains('█'), "orb center: {text:?}");
        assert!(text.contains('▓'), "orb middle: {text:?}");
        assert!(text.contains('▒'), "orb outer band: {text:?}");
        assert!(text.contains('░'), "orb edge: {text:?}");
        assert!(text.contains("breathe in · 3"), "cue: {text:?}");
        assert!(text.contains("take a deep breath."), "lead: {text:?}");
        assert!(
            text.contains("stand up · look at something far away"),
            "tip: {text:?}"
        );

        let center_x = inner.x + ORB_COLUMN_WIDTH / 2;
        let center_y = inner.y + 8;
        let center = &buffer[(center_x, center_y)];
        assert_eq!(center.symbol(), "█");
        assert_eq!(center.style().fg, Some(app.palette.teal));
        assert!(center.style().add_modifier.contains(Modifier::BOLD));

        let middle = &buffer[(center_x + 4, center_y)];
        assert_eq!(middle.symbol(), "▒");
        assert_eq!(middle.style().fg, Some(app.palette.blue));

        let edge = &buffer[(center_x + 6, center_y)];
        assert_eq!(edge.symbol(), "░");
        assert_eq!(edge.style().fg, Some(app.palette.mauve));

        let cue_cells: Vec<_> = (inner.x..inner.x + ORB_COLUMN_WIDTH)
            .map(|x| &buffer[(x, inner.y + 16)])
            .filter(|cell| cell.symbol() != " ")
            .collect();
        assert!(!cue_cells.is_empty(), "breathing cue is rendered");
        assert!(cue_cells
            .iter()
            .all(|cell| cell.style().fg == Some(app.palette.teal)));

        app.view_observed_at = raised_at + std::time::Duration::from_secs(4);
        let hold_buffer = veiled_frame(&app, area);
        let hold_cue_cells: Vec<_> = (inner.x..inner.x + ORB_COLUMN_WIDTH)
            .map(|x| &hold_buffer[(x, inner.y + 16)])
            .filter(|cell| cell.symbol() != " ")
            .collect();
        assert!(!hold_cue_cells.is_empty(), "hold cue is rendered");
        assert!(hold_cue_cells
            .iter()
            .all(|cell| cell.style().fg == Some(app.palette.mauve)));
    }

    #[test]
    fn orb_geometry_threshold_matches_the_drawn_layout_and_input_gate() {
        let mut app = prompted();
        let raised_at = app.pomodoro.prompt.as_ref().expect("prompt").raised_at;
        app.view_observed_at = raised_at + std::time::Duration::from_secs(1);

        let fallback_area = Rect::new(
            0,
            0,
            ORB_PROMPT_WIDTH.saturating_add(3),
            ORB_PROMPT_HEIGHT.saturating_add(2),
        );
        assert!(!orb_layout_available(fallback_area));
        assert_eq!(prompt_size(fallback_area), (PROMPT_WIDTH, PROMPT_HEIGHT));
        let fallback_buffer = veiled_frame(&app, fallback_area);
        let fallback_text: String = fallback_buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(!fallback_text.contains("take a deep breath."));
        assert!(!fallback_text
            .chars()
            .any(|ch| matches!(ch, '█' | '▓' | '▒' | '░')));
        assert!(
            input_presentation_at(&app, fallback_area, app.view_observed_at)
                .prompt
                .is_some()
        );

        let orb_area = Rect::new(
            0,
            0,
            ORB_PROMPT_WIDTH.saturating_add(4),
            ORB_PROMPT_HEIGHT.saturating_add(2),
        );
        assert!(orb_layout_available(orb_area));
        assert_eq!(prompt_size(orb_area), (ORB_PROMPT_WIDTH, ORB_PROMPT_HEIGHT));
        let orb_buffer = veiled_frame(&app, orb_area);
        let orb_text: String = orb_buffer
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(orb_text.contains("take a deep breath."));
        assert!(orb_text
            .chars()
            .any(|ch| matches!(ch, '█' | '▓' | '▒' | '░')));
        assert!(input_presentation_at(&app, orb_area, app.view_observed_at)
            .prompt
            .is_some());
    }

    #[test]
    fn narrow_prompt_keeps_the_existing_layout_and_buttons_without_an_orb() {
        let app = prompted();
        let area = Rect::new(0, 0, 60, 16);
        let buffer = veiled_frame(&app, area);
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(
            !text.contains("take a deep breath."),
            "fallback copy: {text:?}"
        );
        assert!(
            !text.chars().any(|ch| matches!(ch, '█' | '▓' | '▒' | '░')),
            "fallback orb: {text:?}"
        );
        assert!(text.contains("↵ confirm"), "confirm button: {text:?}");
        assert!(text.contains("^⌥b snooze"), "snooze button: {text:?}");
        let (confirm, snooze) = prompt_button_rects(area).expect("fallback buttons");
        assert_eq!(confirm.y, snooze.y);
        assert!(confirm.right() <= area.right());
        assert!(snooze.right() <= area.right());
    }

    #[test]
    fn undersized_prompt_matches_its_input_visibility() {
        let app = prompted();
        let area = Rect::new(0, 0, 60, 10);
        let buffer = veiled_frame(&app, area);
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(
            text.contains("cargo test --all"),
            "an unrenderable prompt must leave the pane visible: {text:?}"
        );
        assert_eq!(
            input_presentation_at(&app, area, std::time::Instant::now()).prompt,
            None,
            "an unrenderable prompt must not claim input"
        );
    }

    #[test]
    fn successful_confirm_draws_the_send_off_card() {
        let started = std::time::Instant::now();
        let prompt_at = started + std::time::Duration::from_secs(25 * 60);
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            started,
        );
        app.pomodoro.tick(prompt_at);
        app.pomodoro.prompt.as_mut().expect("prompt").input = "walk".into();
        app.pomodoro.confirm(prompt_at).expect("confirm");
        app.view_observed_at = prompt_at + std::time::Duration::from_secs(1);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 100, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("enjoy the break"), "title: {text:?}");
        assert!(
            text.contains("stand up · look at something far away"),
            "tip: {text:?}"
        );
        assert!(text.contains("●"), "pulsing dots: {text:?}");
        assert!(text.contains("any key closes"), "dismiss hint: {text:?}");
    }

    #[test]
    fn narrow_fallback_skips_the_send_off_card_instead_of_clipping_it() {
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: PomodoroPhase::ShortBreak,
            shown_at: now,
        });
        app.view_observed_at = now;

        let buffer = veiled_frame(&app, Rect::new(0, 0, 18, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();

        assert!(
            text.contains("cargo test --all"),
            "pane remains visible: {text:?}"
        );
        assert!(
            !text
                .chars()
                .any(|ch| matches!(ch, '┌' | '┐' | '└' | '┘' | '│' | '─')),
            "no clipped card shell: {text:?}"
        );
    }

    #[test]
    fn animation_visibility_matches_the_orb_and_full_send_off_geometry() {
        let now = std::time::Instant::now();
        let app = prompted();

        assert!(!animation_visible_at(&app, Rect::new(0, 0, 60, 16), now));
        assert!(animation_visible_at(&app, Rect::new(0, 0, 100, 30), now));

        let mut app = AppState::test_new();
        app.pomodoro.send_off = Some(crate::pomodoro::PomodoroSendOff {
            started: PomodoroPhase::ShortBreak,
            shown_at: now,
        });
        assert!(!animation_visible_at(&app, Rect::new(0, 0, 18, 30), now));
        assert!(animation_visible_at(&app, Rect::new(0, 0, 100, 30), now));
    }

    #[test]
    fn confirming_a_finished_break_draws_the_welcome_back_send_off() {
        let started = std::time::Instant::now();
        let prompt_at = started + std::time::Duration::from_secs(5 * 60);
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            started,
        );
        app.pomodoro.skip(started);
        app.pomodoro.tick(prompt_at);
        app.pomodoro.prompt.as_mut().expect("prompt").input = "ready".into();
        app.pomodoro.confirm(prompt_at).expect("confirm");
        app.view_observed_at = prompt_at + std::time::Duration::from_secs(1);

        let buffer = veiled_frame(&app, Rect::new(0, 0, 100, 30));
        let text: String = buffer.content().iter().map(|cell| cell.symbol()).collect();
        assert!(text.contains("welcome back"), "title: {text:?}");
        assert!(text.contains("one thing at a time."), "tip: {text:?}");
    }

    /// The reminder is only a reminder if it reaches the frame on top of
    /// whatever else was being drawn.
    #[test]
    fn the_due_reminder_draws_over_the_rest_of_the_frame() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                ..Default::default()
            },
            now,
        );
        app.pomodoro
            .tick(now + std::time::Duration::from_secs(25 * 60));
        if let Some(prompt) = app.pomodoro.prompt.as_mut() {
            prompt.input = "stretching".into();
        }

        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let text: String = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect();
        assert!(text.contains("time for a break"), "overlay title is drawn");
        assert!(text.contains("stretching"), "typed answer is drawn");
    }

    #[test]
    fn the_due_reminder_is_visible_in_every_sidebar_state() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 30;
        let states = [
            (
                false,
                crate::config::SidebarCollapsedModeConfig::Compact,
                "expanded",
            ),
            (
                true,
                crate::config::SidebarCollapsedModeConfig::Compact,
                "compact",
            ),
            (
                true,
                crate::config::SidebarCollapsedModeConfig::Hidden,
                "hidden",
            ),
        ];

        for width in [18, WIDTH] {
            for (collapsed, mode, name) in states {
                let mut app = prompted();
                app.sidebar_collapsed = collapsed;
                app.sidebar_collapsed_mode = mode;
                crate::ui::compute_view(&mut app, Rect::new(0, 0, width, HEIGHT));

                let mut terminal =
                    Terminal::new(TestBackend::new(width, HEIGHT)).expect("test terminal");
                terminal
                    .draw(|frame| crate::ui::render(&app, frame))
                    .expect("render prompt");
                let text: String = terminal
                    .backend()
                    .buffer()
                    .content()
                    .iter()
                    .map(|cell| cell.symbol())
                    .collect();
                let expected_title = if width < 22 {
                    "break due"
                } else {
                    "time for a break"
                };
                assert!(
                    text.contains(expected_title),
                    "{width} columns, {name}: {text:?}"
                );
                assert!(
                    text.contains("↵ confirm"),
                    "{width} columns, {name}: {text:?}"
                );
                assert!(
                    text.contains("^⌥b snooze"),
                    "{width} columns, {name}: {text:?}"
                );
            }
        }
    }

    #[test]
    fn the_countdown_reaches_the_sidebar_footer_row() {
        const WIDTH: u16 = 120;
        const HEIGHT: u16 = 40;
        let now = std::time::Instant::now();
        let mut app = AppState::test_new();
        app.pomodoro = crate::pomodoro::PomodoroState::from_config(
            &crate::config::PomodoroConfig {
                enabled: true,
                sidebar_visible: true,
                ..Default::default()
            },
            now,
        );
        app.sidebar_width = 30;
        crate::ui::compute_view(&mut app, Rect::new(0, 0, WIDTH, HEIGHT));
        let slot = app.view.pomodoro_hit_area;
        assert!(slot.width > 0, "wide sidebar keeps room for the countdown");
        let mut terminal = Terminal::new(TestBackend::new(WIDTH, HEIGHT)).expect("test terminal");
        terminal
            .draw(|frame| crate::ui::render(&app, frame))
            .expect("render");
        let buffer = terminal.backend().buffer();
        let row: String = (slot.x..slot.right())
            .map(|x| buffer[(x, slot.y)].symbol())
            .collect();
        assert!(row.contains("25:0") || row.contains("24:5"), "{row:?}");
    }
}
