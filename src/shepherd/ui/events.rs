//! Events tab (`Eventos (7)`): live event streaming, topic inspector, replay from offset,
//! and pattern runs management with pause/resume and destructive stop confirmation.

use super::components::list_view::{put_str, text_width};
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::{
    buffer::Buffer,
    layout::{Constraint, Direction, Layout, Rect},
    style::{Modifier, Style},
};

use super::components::{
    confirm_stripe::{render_confirm_stripe, ConfirmStripeHits},
    detail_view::{
        render_detail_view, AlertLevel, DetailContentBlock, DetailHeader, DetailViewHits,
    },
    empty_state::{render_empty_state, EmptyStateHits},
    list_view::{render_list_view, ListViewHits, ListViewItem},
};
use crate::app::state::Palette;

pub use shepherd_core::bus::Message as BusMessage;
pub use shepherd_core::service::patterns_run::{PatternRun, PatternRunState, PatternRunSummary};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Default)]
pub enum ShepherdEventsSubTab {
    #[default]
    Topics,
    PatternRuns,
}

impl ShepherdEventsSubTab {
    pub fn label(self) -> &'static str {
        match self {
            Self::Topics => "1. Tópicos y Flujo",
            Self::PatternRuns => "2. Ejecuciones de Patrones",
        }
    }

    pub fn short_label(self) -> &'static str {
        match self {
            Self::Topics => "1. Tópicos",
            Self::PatternRuns => "2. Ejecuciones",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdEventsAction {
    RefreshTopics,
    RefreshRuns,
    ShowRun {
        id: String,
    },
    TailTopic {
        topic: String,
        partition: Option<u32>,
        limit: Option<usize>,
    },
    ReplayTopic {
        topic: String,
        partition: Option<u32>,
        from_offset: u64,
        limit: Option<usize>,
    },
    PauseRun {
        id: String,
    },
    ResumeRun {
        id: String,
    },
    StopRun {
        id: String,
        confirm: bool,
    },
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdTopicRow {
    pub name: String,
    pub partitions: u32,
    pub messages_count: usize,
    pub groups_count: usize,
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdEventsHits {
    pub subtab_rects: Vec<(Rect, ShepherdEventsSubTab)>,
    pub topic_list_hits: ListViewHits,
    pub run_list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub confirm_stripe_hits: Option<ConfirmStripeHits>,
    pub pause_view_button: Option<Rect>,
    pub replay_button: Option<Rect>,
    pub replay_input_rect: Option<Rect>,
    pub filter_input_rect: Option<Rect>,
    /// Message table (wheel scrolls it, not the topic list).
    pub messages_rect: Option<Rect>,
    pub pause_run_button: Option<Rect>,
    pub stop_run_button: Option<Rect>,
    pub empty_hits: Option<EmptyStateHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdEventsSectionState {
    pub active_subtab: ShepherdEventsSubTab,
    pub selected_topic: usize,
    pub topic_scroll: usize,
    pub selected_run: usize,
    pub run_scroll: usize,
    pub topics: Vec<ShepherdTopicRow>,
    pub runs: Vec<PatternRunSummary>,
    pub selected_run_detail: Option<PatternRun>,
    pub messages: Vec<BusMessage>,
    pub message_scroll: usize,
    pub view_paused: bool,
    pub replay_offset_input: String,
    pub agent_filter_input: String,
    pub editing_replay_offset: bool,
    pub editing_agent_filter: bool,
    pub confirm_stop_run: Option<String>,
    pub last_action: Option<ShepherdEventsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdEventsSectionState {
    fn default() -> Self {
        Self {
            active_subtab: ShepherdEventsSubTab::Topics,
            selected_topic: 0,
            topic_scroll: 0,
            selected_run: 0,
            run_scroll: 0,
            topics: Vec::new(),
            runs: Vec::new(),
            selected_run_detail: None,
            messages: Vec::new(),
            message_scroll: 0,
            view_paused: false,
            replay_offset_input: "0".to_string(),
            agent_filter_input: String::new(),
            editing_replay_offset: false,
            editing_agent_filter: false,
            confirm_stop_run: None,
            last_action: None,
            error: None,
        }
    }
}

impl ShepherdEventsSectionState {
    pub fn selected_topic_row(&self) -> Option<&ShepherdTopicRow> {
        self.topics.get(self.selected_topic)
    }

    pub fn selected_run_row(&self) -> Option<&PatternRunSummary> {
        self.runs.get(self.selected_run)
    }

    pub fn select_prev_item(&mut self) {
        match self.active_subtab {
            ShepherdEventsSubTab::Topics => {
                if self.selected_topic > 0 {
                    self.selected_topic -= 1;
                    if self.selected_topic < self.topic_scroll {
                        self.topic_scroll = self.selected_topic;
                    }
                    self.trigger_topic_load();
                }
            }
            ShepherdEventsSubTab::PatternRuns => {
                if self.selected_run > 0 {
                    self.selected_run -= 1;
                    if self.selected_run < self.run_scroll {
                        self.run_scroll = self.selected_run;
                    }
                    self.trigger_run_load();
                }
            }
        }
    }

    pub fn select_next_item(&mut self) {
        match self.active_subtab {
            ShepherdEventsSubTab::Topics => {
                if !self.topics.is_empty() && self.selected_topic + 1 < self.topics.len() {
                    self.selected_topic += 1;
                    self.trigger_topic_load();
                }
            }
            ShepherdEventsSubTab::PatternRuns => {
                if !self.runs.is_empty() && self.selected_run + 1 < self.runs.len() {
                    self.selected_run += 1;
                    self.trigger_run_load();
                }
            }
        }
    }

    /// `up` moves toward older messages; 0 offset follows the live tail.
    pub fn scroll_messages(&mut self, up: bool) {
        self.message_scroll = if up {
            (self.message_scroll + 3).min(self.messages.len())
        } else {
            self.message_scroll.saturating_sub(3)
        };
    }

    pub fn trigger_run_load(&mut self) {
        if let Some(run) = self.selected_run_row() {
            self.last_action = Some(ShepherdEventsAction::ShowRun { id: run.id.clone() });
        }
    }

    pub fn trigger_topic_load(&mut self) {
        if let Some(topic) = self.selected_topic_row() {
            if !self.view_paused {
                self.last_action = Some(ShepherdEventsAction::TailTopic {
                    topic: topic.name.clone(),
                    partition: None,
                    limit: Some(50),
                });
            }
        }
    }

    pub fn trigger_replay(&mut self) {
        if let Some(topic) = self.selected_topic_row() {
            let offset: u64 = self.replay_offset_input.trim().parse().unwrap_or(0);
            self.last_action = Some(ShepherdEventsAction::ReplayTopic {
                topic: topic.name.clone(),
                partition: None,
                from_offset: offset,
                limit: Some(100),
            });
            self.editing_replay_offset = false;
        }
    }

    pub fn toggle_pause_view(&mut self) {
        self.view_paused = !self.view_paused;
        if !self.view_paused {
            self.trigger_topic_load();
        }
    }

    pub fn toggle_pause_selected_run(&mut self) {
        if let Some(run) = self.selected_run_row() {
            if run.state == PatternRunState::Paused {
                self.last_action = Some(ShepherdEventsAction::ResumeRun { id: run.id.clone() });
            } else if run.state == PatternRunState::Running {
                self.last_action = Some(ShepherdEventsAction::PauseRun { id: run.id.clone() });
            }
        }
    }

    pub fn start_stop_run(&mut self) {
        if let Some(run) = self.selected_run_row() {
            if run.state == PatternRunState::Running || run.state == PatternRunState::Paused {
                self.confirm_stop_run = Some(run.id.clone());
            }
        }
    }

    pub fn confirm_stop(&mut self) {
        if let Some(id) = self.confirm_stop_run.take() {
            self.last_action = Some(ShepherdEventsAction::StopRun { id, confirm: true });
        }
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.confirm_stop_run.is_some() {
            self.confirm_stop_run = None;
            return true;
        }
        if self.editing_replay_offset || self.editing_agent_filter {
            self.editing_replay_offset = false;
            self.editing_agent_filter = false;
            return true;
        }
        false
    }

    pub fn handle_key(&mut self, key: KeyEvent) -> bool {
        // Confirmation stripe takes precedence
        if self.confirm_stop_run.is_some() {
            match key.code {
                KeyCode::Enter => {
                    self.confirm_stop();
                    return true;
                }
                KeyCode::Esc => {
                    self.confirm_stop_run = None;
                    return true;
                }
                _ => return true,
            }
        }

        // Inline text inputs
        if self.editing_replay_offset {
            match key.code {
                KeyCode::Enter => {
                    self.trigger_replay();
                    return true;
                }
                KeyCode::Esc => {
                    self.editing_replay_offset = false;
                    return true;
                }
                KeyCode::Backspace => {
                    self.replay_offset_input.pop();
                    return true;
                }
                KeyCode::Char(c) if c.is_ascii_digit() => {
                    self.replay_offset_input.push(c);
                    return true;
                }
                _ => return true,
            }
        }

        if self.editing_agent_filter {
            match key.code {
                KeyCode::Enter | KeyCode::Esc => {
                    self.editing_agent_filter = false;
                    return true;
                }
                KeyCode::Backspace => {
                    self.agent_filter_input.pop();
                    return true;
                }
                KeyCode::Char(c) => {
                    self.agent_filter_input.push(c);
                    return true;
                }
                _ => return true,
            }
        }

        // Standard navigation
        match key.code {
            KeyCode::Tab | KeyCode::BackTab => {
                self.active_subtab = match self.active_subtab {
                    ShepherdEventsSubTab::Topics => ShepherdEventsSubTab::PatternRuns,
                    ShepherdEventsSubTab::PatternRuns => ShepherdEventsSubTab::Topics,
                };
                if self.active_subtab == ShepherdEventsSubTab::Topics {
                    self.trigger_topic_load();
                } else {
                    self.trigger_run_load();
                }
                true
            }
            KeyCode::Char('1') => {
                self.active_subtab = ShepherdEventsSubTab::Topics;
                self.trigger_topic_load();
                true
            }
            KeyCode::Char('2') => {
                self.active_subtab = ShepherdEventsSubTab::PatternRuns;
                true
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev_item();
                true
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next_item();
                true
            }
            KeyCode::PageUp => {
                self.scroll_messages(true);
                true
            }
            KeyCode::PageDown => {
                self.scroll_messages(false);
                true
            }
            KeyCode::Char(' ') => {
                if self.active_subtab == ShepherdEventsSubTab::Topics {
                    self.toggle_pause_view();
                    true
                } else {
                    false
                }
            }
            KeyCode::Char('p') => {
                match self.active_subtab {
                    ShepherdEventsSubTab::Topics => self.toggle_pause_view(),
                    ShepherdEventsSubTab::PatternRuns => self.toggle_pause_selected_run(),
                }
                true
            }
            KeyCode::Char('r') => {
                match self.active_subtab {
                    ShepherdEventsSubTab::Topics => {
                        self.editing_replay_offset = true;
                    }
                    ShepherdEventsSubTab::PatternRuns => {
                        self.toggle_pause_selected_run();
                    }
                }
                true
            }
            KeyCode::Char('/') => {
                if self.active_subtab == ShepherdEventsSubTab::Topics {
                    self.editing_agent_filter = true;
                    true
                } else {
                    false
                }
            }
            KeyCode::Char('x') => {
                if self.active_subtab == ShepherdEventsSubTab::PatternRuns {
                    self.start_stop_run();
                    true
                } else {
                    false
                }
            }
            KeyCode::Esc => self.cancel_sub_action(),
            _ => false,
        }
    }
}

pub fn render_events_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &mut ShepherdEventsSectionState,
    palette: &Palette,
) -> ShepherdEventsHits {
    let mut hits = ShepherdEventsHits::default();
    if area.width < 10 || area.height < 6 {
        return hits;
    }

    // Top bar: sub-tabs
    let layout = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(1), // Sub-tab bar
            Constraint::Length(1), // Divider line
            Constraint::Min(4),    // Main contents
        ])
        .split(area);

    let subtab_bar_area = layout[0];
    let divider_area = layout[1];
    let content_area = layout[2];

    // 1. Render Subtab Bar
    render_subtab_bar(
        buffer,
        subtab_bar_area,
        state.active_subtab,
        &mut hits,
        palette,
    );

    // Divider
    let div_style = Style::default().fg(palette.surface_dim);
    for x in divider_area.left()..divider_area.right() {
        buffer[(x, divider_area.top())]
            .set_char('─')
            .set_style(div_style);
    }

    // If confirmation stripe active
    let (main_area, stripe_area) = if let Some(ref run_id) = state.confirm_stop_run {
        let run_summary = state.runs.iter().find(|r| &r.id == run_id);
        let agents_count = run_summary.map(|r| r.agents_count).unwrap_or(0);
        let prompt = format!(
            "¿Detener ejecución '{run_id}' y matar sus {agents_count} agentes? Esta acción no se puede deshacer."
        );
        let parts = Layout::default()
            .direction(Direction::Vertical)
            .constraints([Constraint::Min(4), Constraint::Length(3)])
            .split(content_area);

        let s_hits = render_confirm_stripe(
            buffer,
            parts[1],
            &prompt,
            "Confirmar detención (Enter/d)",
            "Cancelar (Esc)",
            true,
            palette,
        );
        hits.confirm_stripe_hits = Some(s_hits);
        (parts[0], Some(parts[1]))
    } else {
        (content_area, None)
    };
    let _ = stripe_area;

    match state.active_subtab {
        ShepherdEventsSubTab::Topics => {
            render_topics_view(buffer, main_area, state, &mut hits, palette);
        }
        ShepherdEventsSubTab::PatternRuns => {
            render_pattern_runs_view(buffer, main_area, state, &mut hits, palette);
        }
    }

    hits
}

fn render_subtab_bar(
    buffer: &mut Buffer,
    area: Rect,
    active: ShepherdEventsSubTab,
    hits: &mut ShepherdEventsHits,
    palette: &Palette,
) {
    let subtabs = [
        ShepherdEventsSubTab::Topics,
        ShepherdEventsSubTab::PatternRuns,
    ];
    let mut current_x = area.left() + 1;

    for tab in subtabs {
        let label = if area.width >= 64 {
            tab.label()
        } else {
            tab.short_label()
        };
        let is_active = tab == active;
        let text = format!("[ {label} ]");
        let width = text_width(&text);

        if current_x + width > area.right() {
            break;
        }

        let style = if is_active {
            Style::default()
                .fg(palette.accent)
                .add_modifier(Modifier::BOLD)
        } else {
            Style::default().fg(palette.overlay0)
        };

        buffer.set_string(current_x, area.top(), &text, style);
        hits.subtab_rects.push((
            Rect {
                x: current_x,
                y: area.top(),
                width,
                height: 1,
            },
            tab,
        ));
        current_x += width + 2;
    }
}

fn render_topics_view(
    buffer: &mut Buffer,
    area: Rect,
    state: &mut ShepherdEventsSectionState,
    hits: &mut ShepherdEventsHits,
    palette: &Palette,
) {
    if state.topics.is_empty() {
        hits.empty_hits = Some(render_empty_state(
            buffer,
            area,
            "SIN TÓPICOS ACTIVOS",
            "No se han creado tópicos en el bus de eventos aún.\nInicia un patrón de agentes o usa 'shepherd topic create' para comenzar.",
            &[("+ Ver Patrones", Some("4"))],
            palette,
        ));
        return;
    }

    // Split left (topics list) and right (topic messages & controls)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);

    let left_area = chunks[0];
    let right_area = chunks[1];

    // Left: Topics list
    let list_items: Vec<ListViewItem> = state
        .topics
        .iter()
        .map(|t| ListViewItem {
            title: t.name.clone(),
            badge: Some((
                format!("{}p", t.partitions),
                Style::default().fg(palette.accent),
            )),
            meta: Some(format!("{} msgs", t.messages_count)),
        })
        .collect();

    hits.topic_list_hits = render_list_view(
        buffer,
        left_area,
        &list_items,
        state.selected_topic,
        state.topic_scroll,
        None,
        None,
        palette,
    );

    // Right: Controls bar + Message stream table
    let right_chunks = Layout::default()
        .direction(Direction::Vertical)
        .constraints([
            Constraint::Length(2), // Controls row: [ Pause/Resume ] [ Replay offset ] [ Filter ]
            Constraint::Length(1), // Header row
            Constraint::Min(2),    // Messages rows
        ])
        .split(right_area);

    let controls_area = right_chunks[0];
    let header_area = right_chunks[1];
    let table_area = right_chunks[2];

    // Controls: flow left to right, wrapping to the second row when narrow; what
    // still does not fit is not drawn (and gets no hit rect).
    let mut cx = controls_area.left();
    let mut cy = controls_area.top();
    let mut place = |w: u16, gap: u16| -> Option<(u16, u16)> {
        if cx + w > controls_area.right() {
            cx = controls_area.left();
            cy += 1;
        }
        if cy >= controls_area.bottom() || cx + w > controls_area.right() {
            return None;
        }
        let at = (cx, cy);
        cx += w + gap;
        Some(at)
    };

    let pause_label = if state.view_paused {
        "[ ⏸ Pausada (p) ]"
    } else {
        "[ ▶ En vivo (p) ]"
    };
    let pause_w = text_width(pause_label);
    let pause_style = Style::default()
        .fg(if state.view_paused {
            palette.yellow
        } else {
            palette.green
        })
        .add_modifier(Modifier::BOLD);
    if let Some((x, y)) = place(pause_w, 2) {
        put_str(buffer, x, y, pause_w, pause_label, pause_style);
        hits.pause_view_button = Some(Rect {
            x,
            y,
            width: pause_w,
            height: 1,
        });
    }

    let replay_input_str = format!("Offset: [{}]", state.replay_offset_input);
    let replay_in_w = text_width(&replay_input_str);
    let replay_in_style = if state.editing_replay_offset {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(palette.text)
    };
    if let Some((x, y)) = place(replay_in_w, 1) {
        put_str(
            buffer,
            x,
            y,
            replay_in_w,
            &replay_input_str,
            replay_in_style,
        );
        hits.replay_input_rect = Some(Rect {
            x,
            y,
            width: replay_in_w,
            height: 1,
        });
    }

    let replay_btn = "[ ⟲ Replay (r) ]";
    let replay_btn_w = text_width(replay_btn);
    if let Some((x, y)) = place(replay_btn_w, 2) {
        put_str(
            buffer,
            x,
            y,
            replay_btn_w,
            replay_btn,
            Style::default().fg(palette.accent),
        );
        hits.replay_button = Some(Rect {
            x,
            y,
            width: replay_btn_w,
            height: 1,
        });
    }

    let filter_str = if state.agent_filter_input.is_empty() {
        "Filtro: [ / ]".to_string()
    } else {
        format!("Filtro: [{}]", state.agent_filter_input)
    };
    let filter_w = text_width(&filter_str);
    let filter_style = if state.editing_agent_filter {
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::REVERSED)
    } else {
        Style::default().fg(palette.overlay0)
    };
    if let Some((x, y)) = place(filter_w, 0) {
        put_str(buffer, x, y, filter_w, &filter_str, filter_style);
        hits.filter_input_rect = Some(Rect {
            x,
            y,
            width: filter_w,
            height: 1,
        });
    }

    // Header Row
    let head_style = Style::default()
        .fg(palette.accent)
        .add_modifier(Modifier::BOLD);
    let head_text = format!(
        "{:<6} {:<4} {:<12} {:<10} {:<6} {:<24}",
        "OFFSET", "PART", "DE", "CLAVE", "TIPO", "CONTENIDO"
    );
    put_str(
        buffer,
        header_area.left(),
        header_area.top(),
        header_area.width,
        &head_text,
        head_style,
    );

    // Messages table
    let filter_term = state.agent_filter_input.trim().to_lowercase();
    let filtered_messages: Vec<&BusMessage> = state
        .messages
        .iter()
        .filter(|m| {
            if filter_term.is_empty() {
                true
            } else {
                m.from.to_lowercase().contains(&filter_term)
                    || m.to.to_lowercase().contains(&filter_term)
                    || m.content.to_lowercase().contains(&filter_term)
            }
        })
        .collect();

    hits.messages_rect = Some(table_area);
    let visible_rows = table_area.height as usize;
    // message_scroll = rows above the newest message (0 follows the live tail).
    let total = filtered_messages.len();
    let end_idx = total - state.message_scroll.min(total.saturating_sub(1));
    let start_idx = end_idx.saturating_sub(visible_rows);

    for (row_idx, m) in filtered_messages[start_idx..end_idx].iter().enumerate() {
        let y = table_area.top() + row_idx as u16;
        let excerpt = m.content.replace('\n', " ");
        let line_text = format!(
            "{:<6} {:<4} {:<12} {:<10} {:<6} {}",
            m.offset,
            m.partition,
            truncate_str(&m.from, 12),
            truncate_str(&m.key, 10),
            truncate_str(&m.kind, 6),
            excerpt
        );
        let row_style = Style::default().fg(palette.text);
        put_str(
            buffer,
            table_area.left(),
            y,
            table_area.width,
            &line_text,
            row_style,
        );
    }
}

fn render_pattern_runs_view(
    buffer: &mut Buffer,
    area: Rect,
    state: &mut ShepherdEventsSectionState,
    hits: &mut ShepherdEventsHits,
    palette: &Palette,
) {
    if state.runs.is_empty() {
        hits.empty_hits = Some(render_empty_state(
            buffer,
            area,
            "SIN EJECUCIONES DE PATRONES",
            "No hay ejecuciones de patrones en curso ni archivadas.\nVe a la pestaña Patrones (4) y presiona [ Ejecutar (r) ] para instanciar uno.",
            &[("+ Ver Patrones", Some("4"))],
            palette,
        ));
        return;
    }

    // Split left (runs list) and right (run detail)
    let chunks = Layout::default()
        .direction(Direction::Horizontal)
        .constraints([Constraint::Percentage(32), Constraint::Percentage(68)])
        .split(area);

    let left_area = chunks[0];
    let right_area = chunks[1];

    let list_items: Vec<ListViewItem> = state
        .runs
        .iter()
        .map(|r| {
            let (state_color, state_badge) = match r.state {
                PatternRunState::Running => (palette.green, "RUNNING"),
                PatternRunState::Paused => (palette.yellow, "PAUSED"),
                PatternRunState::Stopped => (palette.red, "STOPPED"),
                PatternRunState::Failed => (palette.red, "FAILED"),
                PatternRunState::Done => (palette.blue, "DONE"),
                PatternRunState::Degraded => (palette.yellow, "DEGRADED"),
            };
            ListViewItem {
                title: r.pattern.clone(),
                badge: Some((state_badge.to_string(), Style::default().fg(state_color))),
                meta: Some(format!("{} ag | {}", r.agents_count, r.id)),
            }
        })
        .collect();

    hits.run_list_hits = render_list_view(
        buffer,
        left_area,
        &list_items,
        state.selected_run,
        state.run_scroll,
        None,
        None,
        palette,
    );

    // Right: Detail view
    if let Some(ref detail) = state.selected_run_detail {
        let (status_color, status_label) = match detail.state {
            PatternRunState::Running => (palette.green, "✓ EN EJECUCIÓN"),
            PatternRunState::Paused => (palette.yellow, "⏸ PAUSADA"),
            PatternRunState::Stopped => (palette.red, "⏹ DETENIDA"),
            PatternRunState::Failed => (palette.red, "✗ ERROR EN EJECUCIÓN"),
            PatternRunState::Done if detail.quiesced => (
                palette.blue,
                "✓ COMPLETADA (en reposo; despierta con mensaje nuevo)",
            ),
            PatternRunState::Done => (palette.blue, "✓ COMPLETADA"),
            PatternRunState::Degraded => (palette.yellow, "⚠ DEGRADADA"),
        };

        let header = DetailHeader {
            title: format!("EJECUCIÓN: {}", detail.id),
            badges: vec![
                (status_label.to_string(), Style::default().fg(status_color)),
                (
                    format!("Topología: {}", detail.kind),
                    Style::default().fg(palette.accent),
                ),
            ],
            metadata_line: Some(format!(
                "Patrón: {} | Iniciada: {}",
                detail.pattern, detail.created_at
            )),
            actions: vec![
                ("Pausar/Reanudar".into(), Some("p".into())),
                ("Detener".into(), Some("x".into())),
            ],
        };

        let task_desc = if detail.task.trim().is_empty() {
            "-"
        } else {
            &detail.task
        };
        let mut blocks = vec![
            DetailContentBlock::Heading("INFORMACIÓN".into()),
            DetailContentBlock::Paragraph(format!(
                "Tarea / Objetivo: {}\nTópicos: {}",
                task_desc,
                detail.topics.join(", ")
            )),
        ];

        if let Some(ref wt) = detail.worktree {
            blocks.push(DetailContentBlock::Paragraph(format!("Worktree: {wt}")));
        }

        if let Some(ref err) = detail.error {
            blocks.push(DetailContentBlock::Notice {
                text: format!("Fallo: {err}"),
                level: AlertLevel::Warning,
            });
        }

        // Agents list
        let mut ag_lines = Vec::new();
        for ag in &detail.agents {
            let lvl_str = ag.level.map(|l| format!(" (lvl {l})")).unwrap_or_default();
            ag_lines.push(format!(
                "• {:<12} [{}] {:<8} {:<8} {} (pub: {:?}, sub: {:?})",
                ag.id,
                ag.state,
                ag.role,
                ag.vendor.as_deref().unwrap_or("-"),
                lvl_str,
                ag.publishes,
                ag.consumes
            ));
        }
        blocks.push(DetailContentBlock::Heading(format!(
            "AGENTES ({})",
            detail.agents.len()
        )));
        blocks.push(DetailContentBlock::BulletList(ag_lines));

        let is_running = detail.state == PatternRunState::Running;
        let is_paused = detail.state == PatternRunState::Paused;

        let pause_resume_action = if is_running {
            ("Pausar", "p", true)
        } else if is_paused {
            ("Reanudar", "p", true)
        } else {
            ("Pausar", "", false)
        };

        let can_stop = is_running || is_paused;
        let stop_action = ("Detener", "x", can_stop);

        let bottom_actions = [pause_resume_action, stop_action];

        hits.detail_hits = Some(render_detail_view(
            buffer,
            right_area,
            &header,
            &blocks,
            0,
            &bottom_actions,
            palette,
        ));
        // Pause/resume = action 0, stop = action 1; bottom buttons win over header ones.
        if let Some(dh) = &hits.detail_hits {
            let find = |idx: usize| {
                dh.bottom_actions
                    .iter()
                    .chain(dh.header_actions.iter())
                    .find(|&&(_, i)| i == idx)
                    .map(|&(r, _)| r)
            };
            hits.pause_run_button = find(0);
            hits.stop_run_button = find(1);
        }
    }
}

fn truncate_str(s: &str, max_len: usize) -> String {
    if s.chars().count() <= max_len {
        s.to_string()
    } else {
        let prefix: String = s.chars().take(max_len.saturating_sub(1)).collect();
        format!("{prefix}…")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn test_events_section_state_machine_and_subtabs() {
        let mut state = ShepherdEventsSectionState::default();
        assert_eq!(state.active_subtab, ShepherdEventsSubTab::Topics);

        // Switch to PatternRuns via key '2'
        state.handle_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(state.active_subtab, ShepherdEventsSubTab::PatternRuns);

        // Switch back to Topics via key '1'
        state.handle_key(KeyEvent::from(KeyCode::Char('1')));
        assert_eq!(state.active_subtab, ShepherdEventsSubTab::Topics);
    }

    #[test]
    fn test_events_section_pure_render_and_empty_states() {
        let mut state = ShepherdEventsSectionState::default();
        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let hits = render_events_section(&mut buffer, area, &mut state, &palette);
        assert_eq!(hits.subtab_rects.len(), 2);
        assert!(hits.empty_hits.is_some());
    }

    #[test]
    fn test_events_section_topic_view_with_messages_and_filters() {
        let mut state = ShepherdEventsSectionState::default();
        state.topics.push(ShepherdTopicRow {
            name: "tasks-assigned".into(),
            partitions: 4,
            messages_count: 2,
            groups_count: 1,
        });
        state.messages.push(BusMessage {
            id: 1,
            ts: "2026-11-05T12:00:00Z".into(),
            from: "planner".into(),
            to: "".into(),
            kind: "msg".into(),
            topic: "tasks-assigned".into(),
            partition: 0,
            offset: 0,
            key: "task-1".into(),
            content: "Implement login view".into(),
        });
        state.messages.push(BusMessage {
            id: 2,
            ts: "2026-11-05T12:01:00Z".into(),
            from: "coder".into(),
            to: "".into(),
            kind: "msg".into(),
            topic: "tasks-assigned".into(),
            partition: 1,
            offset: 0,
            key: "task-2".into(),
            content: "Build user service".into(),
        });

        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let hits = render_events_section(&mut buffer, area, &mut state, &palette);
        assert!(hits.empty_hits.is_none());
        assert_eq!(hits.topic_list_hits.rows.len(), 1);
        assert!(hits.pause_view_button.is_some());
        assert!(hits.replay_button.is_some());

        // Toggle pause view
        state.handle_key(KeyEvent::from(KeyCode::Char('p')));
        assert!(state.view_paused);
        state.handle_key(KeyEvent::from(KeyCode::Char('p')));
        assert!(!state.view_paused);

        // Filter by agent
        state.agent_filter_input = "planner".into();
        let _hits_filtered = render_events_section(&mut buffer, area, &mut state, &palette);
    }

    #[test]
    fn test_events_section_pattern_runs_view_and_stop_guard() {
        let mut state = ShepherdEventsSectionState::default();
        state.active_subtab = ShepherdEventsSubTab::PatternRuns;
        state.runs.push(PatternRunSummary {
            id: "run-ow-101".into(),
            pattern: "orchestrator-worker".into(),
            kind: "orchestrator-worker".into(),
            state: PatternRunState::Running,
            agents_count: 4,
            live_agents: 4,
            topics_count: 2,
            created_at: "2026-11-05T12:00:00Z".into(),
            updated_at: "2026-11-05T12:00:00Z".into(),
        });
        state.selected_run_detail = Some(PatternRun {
            id: "run-ow-101".into(),
            pattern: "orchestrator-worker".into(),
            kind: "orchestrator-worker".into(),
            task: "Build microservice".into(),
            worktree: None,
            topics: vec!["tasks-assigned".into(), "tasks-done".into()],
            agents: vec![],
            state: PatternRunState::Running,
            created_at: "2026-11-05T12:00:00Z".into(),
            updated_at: "2026-11-05T12:00:00Z".into(),
            finished_at: None,
            error: None,
            quiesced: false,
        });

        let area = Rect::new(0, 0, 100, 30);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let hits = render_events_section(&mut buffer, area, &mut state, &palette);
        assert!(hits.empty_hits.is_none());
        assert_eq!(hits.run_list_hits.rows.len(), 1);
        assert!(hits.detail_hits.is_some());

        // Press 'p' to trigger pause run action
        state.handle_key(KeyEvent::from(KeyCode::Char('p')));
        assert!(matches!(
            state.last_action,
            Some(ShepherdEventsAction::PauseRun { ref id }) if id == "run-ow-101"
        ));

        // Press 'x' to initiate stop run -> opens confirmation stripe
        state.handle_key(KeyEvent::from(KeyCode::Char('x')));
        assert_eq!(state.confirm_stop_run.as_deref(), Some("run-ow-101"));

        // Render with confirmation stripe
        let hits_stripe = render_events_section(&mut buffer, area, &mut state, &palette);
        assert!(hits_stripe.confirm_stripe_hits.is_some());

        // Confirm with Enter
        state.handle_key(KeyEvent::from(KeyCode::Enter));
        assert!(matches!(
            state.last_action,
            Some(ShepherdEventsAction::StopRun { ref id, confirm: true }) if id == "run-ow-101"
        ));
        assert!(state.confirm_stop_run.is_none());
    }

    #[test]
    fn test_events_section_80x24_pure_render() {
        let mut state = ShepherdEventsSectionState::default();
        let area = Rect::new(0, 0, 80, 24);
        let mut buffer = Buffer::empty(area);
        let palette = Palette::catppuccin();

        let hits = render_events_section(&mut buffer, area, &mut state, &palette);
        assert!(hits.empty_hits.is_some());
    }
}
