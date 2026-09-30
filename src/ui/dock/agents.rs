//! Focus-following Claude subagent tree.
//!
//! The existing Claude transcript refresh owns the observations. This module
//! only projects the focused pane's snapshot and attach-local interaction.

use std::collections::HashSet;
use std::time::{Duration, SystemTime};

use ratatui::{
    layout::Rect,
    style::{Color, Modifier, Style},
    text::{Line, Span},
    widgets::Paragraph,
    Frame,
};

use crate::app::claude_subagents::ClaudeSubagentObservation;
use crate::app::state::{AppState, DockAgentRowHitArea};
use crate::detect::AgentState;

const HEADER_ROWS: u16 = 1;

#[derive(Clone, Copy, Debug)]
pub(crate) struct AgentTreeRow<'a> {
    pub(crate) observation: &'a ClaudeSubagentObservation,
    pub(crate) depth: usize,
}

pub(crate) fn focused_observations(app: &AppState) -> &[ClaudeSubagentObservation] {
    let Some(workspace) = app.active.and_then(|index| app.workspaces.get(index)) else {
        return &[];
    };
    let Some(terminal) = workspace
        .focused_pane_id()
        .and_then(|pane_id| workspace.terminal_id(pane_id))
        .and_then(|terminal_id| app.terminals.get(terminal_id))
    else {
        return &[];
    };
    if terminal.claude_transcript_session_id.is_none() {
        return &[];
    }
    terminal
        .claude_subagent_observations
        .as_deref()
        .unwrap_or_default()
}

pub(crate) fn has_focused_observations(app: &AppState) -> bool {
    !focused_observations(app).is_empty()
}

pub(crate) fn ordered_rows(observations: &[ClaudeSubagentObservation]) -> Vec<AgentTreeRow<'_>> {
    let ids = observations
        .iter()
        .map(|observation| observation.id.as_str())
        .collect::<HashSet<_>>();
    let mut visited = HashSet::new();
    let mut rows = Vec::with_capacity(observations.len());

    for observation in observations.iter().filter(|observation| {
        observation
            .parent_id
            .as_deref()
            .is_none_or(|parent| !ids.contains(parent))
    }) {
        append_branch(observations, observation, 0, &mut visited, &mut rows);
    }
    // Cycles and malformed parent links remain visible in source order.
    for observation in observations {
        if !visited.contains(observation.id.as_str()) {
            append_branch(observations, observation, 0, &mut visited, &mut rows);
        }
    }
    rows
}

fn append_branch<'a>(
    observations: &'a [ClaudeSubagentObservation],
    observation: &'a ClaudeSubagentObservation,
    depth: usize,
    visited: &mut HashSet<&'a str>,
    rows: &mut Vec<AgentTreeRow<'a>>,
) {
    if !visited.insert(observation.id.as_str()) {
        return;
    }
    rows.push(AgentTreeRow { observation, depth });
    for child in observations
        .iter()
        .filter(|candidate| candidate.parent_id.as_deref() == Some(observation.id.as_str()))
    {
        append_branch(observations, child, depth.saturating_add(1), visited, rows);
    }
}

pub(crate) fn row_hit_areas(app: &AppState, area: Rect) -> Vec<DockAgentRowHitArea> {
    if area.width == 0 || area.height <= HEADER_ROWS {
        return Vec::new();
    }
    let rows = ordered_rows(focused_observations(app));
    let height = usize::from(area.height - HEADER_ROWS);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    rows.into_iter()
        .skip(scroll)
        .take(height)
        .enumerate()
        .map(|(index, row)| DockAgentRowHitArea {
            id: row.observation.id.clone(),
            rect: Rect::new(
                area.x,
                area.y + HEADER_ROWS + u16::try_from(index).unwrap_or(u16::MAX),
                area.width,
                1,
            ),
        })
        .collect()
}

pub(crate) fn render_agents(app: &AppState, frame: &mut Frame, area: Rect) {
    render_agents_at(app, frame, area, SystemTime::now());
}

fn render_agents_at(app: &AppState, frame: &mut Frame, area: Rect, now: SystemTime) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let rows = ordered_rows(focused_observations(app));
    let header = if rows.is_empty() {
        " no subagents".to_string()
    } else {
        format!(" subagents  {}", rows.len())
    };
    frame.render_widget(
        Paragraph::new(header).style(Style::default().fg(app.palette.overlay1)),
        Rect::new(area.x, area.y, area.width, 1),
    );
    if area.height <= HEADER_ROWS {
        return;
    }

    let height = usize::from(area.height - HEADER_ROWS);
    let scroll = usize::from(app.dock_scroll).min(rows.len().saturating_sub(height));
    for (index, row) in rows.iter().skip(scroll).take(height).enumerate() {
        render_row(
            app,
            frame,
            Rect::new(
                area.x,
                area.y + HEADER_ROWS + u16::try_from(index).unwrap_or(u16::MAX),
                area.width,
                1,
            ),
            *row,
            now,
        );
    }
}

fn render_row(
    app: &AppState,
    frame: &mut Frame,
    area: Rect,
    row: AgentTreeRow<'_>,
    now: SystemTime,
) {
    let observation = row.observation;
    let selected = app.dock_agents_selection.as_deref() == Some(observation.id.as_str());
    let row_style = if selected && app.dock_agents_focused {
        Style::default()
            .fg(app.palette.text)
            .bg(app.palette.surface0)
            .add_modifier(Modifier::BOLD)
    } else {
        Style::default().fg(app.palette.text)
    };
    let (glyph, label, color) = status_class(observation.state, app);
    let age = age_label(observation.observed_at, now);
    let prefix = format!(" {}{glyph} ", "  ".repeat(row.depth));
    let suffix = format!("  {label}  {age}");
    let name_width = usize::from(area.width)
        .saturating_sub(prefix.chars().count())
        .saturating_sub(suffix.chars().count());
    let name = observation
        .name
        .chars()
        .take(name_width)
        .collect::<String>();
    let padding = usize::from(area.width)
        .saturating_sub(prefix.chars().count() + name.chars().count() + suffix.chars().count());
    let line = Line::from(vec![
        Span::styled(prefix, row_style.patch(Style::default().fg(color))),
        Span::styled(name, row_style),
        Span::raw(" ".repeat(padding)),
        Span::styled(suffix, row_style.patch(Style::default().fg(color))),
    ]);
    frame.render_widget(Paragraph::new(line).style(row_style), area);
}

fn status_class(state: AgentState, app: &AppState) -> (&'static str, &'static str, Color) {
    let label = match state {
        AgentState::Working => "running",
        AgentState::Idle => "done",
        AgentState::Blocked => "blocked",
        AgentState::Unknown => "unknown",
    };
    (
        crate::ui::sidebar::compact_dot_for_state(state, false, true, false, false, false),
        label,
        crate::ui::status::state_label_color(state, false, &app.palette),
    )
}

fn age_label(then: Option<SystemTime>, now: SystemTime) -> String {
    let elapsed = then
        .and_then(|then| now.duration_since(then).ok())
        .unwrap_or(Duration::ZERO);
    if elapsed.as_secs() >= 86_400 {
        format!("{}d", elapsed.as_secs() / 86_400)
    } else if elapsed.as_secs() >= 3_600 {
        format!("{}h", elapsed.as_secs() / 3_600)
    } else {
        format!("{}m", elapsed.as_secs() / 60)
    }
}

impl AppState {
    pub(crate) fn reconcile_dock_agents_selection(&mut self) {
        let rows = ordered_rows(focused_observations(self));
        let row_count = rows.len();
        let replacement = (!rows
            .iter()
            .any(|row| self.dock_agents_selection.as_deref() == Some(row.observation.id.as_str())))
        .then(|| rows.first().map(|row| row.observation.id.clone()));
        if let Some(replacement) = replacement {
            self.dock_agents_selection = replacement;
        }
        self.keep_dock_agents_selection_visible(row_count);
    }

    pub(crate) fn move_dock_agents_selection(&mut self, delta: isize) {
        let rows = ordered_rows(focused_observations(self));
        if rows.is_empty() {
            self.dock_agents_selection = None;
            self.dock_scroll = 0;
            return;
        }
        let current = self
            .dock_agents_selection
            .as_deref()
            .and_then(|id| rows.iter().position(|row| row.observation.id == id))
            .unwrap_or(0);
        let next = current.saturating_add_signed(delta).min(rows.len() - 1);
        let selection = rows[next].observation.id.clone();
        let row_count = rows.len();
        self.dock_agents_selection = Some(selection);
        self.keep_dock_agents_selection_visible(row_count);
    }

    fn keep_dock_agents_selection_visible(&mut self, row_count: usize) {
        let rows = ordered_rows(focused_observations(self));
        let Some(index) = self
            .dock_agents_selection
            .as_deref()
            .and_then(|id| rows.iter().position(|row| row.observation.id == id))
        else {
            self.dock_scroll = 0;
            return;
        };
        let height = usize::from(
            self.view
                .dock_body_rect
                .height
                .saturating_sub(HEADER_ROWS)
                .max(1),
        );
        let max_scroll = row_count.saturating_sub(height);
        let current = usize::from(self.dock_scroll).min(max_scroll);
        let scroll = if index < current {
            index
        } else if index >= current + height {
            index + 1 - height
        } else {
            current
        };
        self.dock_scroll = u16::try_from(scroll.min(max_scroll)).unwrap_or(u16::MAX);
    }

    pub(crate) fn scroll_dock_agents(&mut self, delta: isize) {
        let row_count = ordered_rows(focused_observations(self)).len();
        let height = usize::from(
            self.view
                .dock_body_rect
                .height
                .saturating_sub(HEADER_ROWS)
                .max(1),
        );
        let max_scroll = row_count.saturating_sub(height);
        let next = usize::from(self.dock_scroll)
            .saturating_add_signed(delta)
            .min(max_scroll);
        self.dock_scroll = u16::try_from(next).unwrap_or(u16::MAX);
    }

    pub(crate) fn selected_dock_agent_observation(&self) -> Option<&ClaudeSubagentObservation> {
        let rows = ordered_rows(focused_observations(self));
        let selected = self.dock_agents_selection.as_deref();
        rows.iter()
            .find(|row| selected == Some(row.observation.id.as_str()))
            .or_else(|| rows.first())
            .map(|row| row.observation)
    }

    pub(crate) fn click_dock_agent_row(&mut self, col: u16, row: u16) -> bool {
        let Some(hit) = self
            .view
            .dock_agent_row_hit_areas
            .iter()
            .find(|hit| {
                col >= hit.rect.x
                    && col < hit.rect.right()
                    && row >= hit.rect.y
                    && row < hit.rect.bottom()
            })
            .cloned()
        else {
            return false;
        };
        self.dock_agents_selection = Some(hit.id);
        self.dock_agents_focused = true;
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::{backend::TestBackend, Terminal};
    use std::path::PathBuf;

    fn observation(
        id: &str,
        parent_id: Option<&str>,
        name: &str,
        state: AgentState,
        age_minutes: u64,
    ) -> ClaudeSubagentObservation {
        ClaudeSubagentObservation {
            id: id.into(),
            parent_id: parent_id.map(str::to_string),
            name: name.into(),
            state,
            observed_at: Some(SystemTime::UNIX_EPOCH + Duration::from_secs(age_minutes * 60)),
            transcript_path: Some(PathBuf::from(format!("/tmp/{id}.jsonl"))),
        }
    }

    fn app_with(observations: Vec<ClaudeSubagentObservation>) -> AppState {
        let mut app = AppState::test_new();
        app.workspaces = vec![crate::workspace::Workspace::test_new("claude")];
        app.active = Some(0);
        app.ensure_test_terminals();
        let terminal_id = app.workspaces[0]
            .focused_pane_id()
            .and_then(|pane_id| app.workspaces[0].terminal_id(pane_id))
            .expect("focused terminal")
            .clone();
        let terminal = app.terminals.get_mut(&terminal_id).expect("terminal state");
        terminal.claude_transcript_session_id = Some("session".into());
        terminal.claude_subagent_observations = Some(observations);
        app.dock_collapsed = false;
        app.dock_tab = Some(crate::app::DockSurface::Agents);
        app.dock_agents_focused = true;
        app
    }

    #[test]
    fn tree_order_keeps_each_parent_before_its_children() {
        let observations = vec![
            observation("a", None, "research", AgentState::Working, 1),
            observation("b", None, "tests", AgentState::Idle, 2),
            observation("a2", Some("a"), "nested", AgentState::Blocked, 3),
            observation("a1", Some("a"), "review", AgentState::Working, 4),
        ];
        let rows = ordered_rows(&observations);

        assert_eq!(
            rows.iter()
                .map(|row| (row.observation.id.as_str(), row.depth))
                .collect::<Vec<_>>(),
            vec![("a", 0), ("a2", 1), ("a1", 1), ("b", 0)]
        );
    }

    #[test]
    fn availability_requires_a_focused_claude_observation() {
        let mut app = app_with(Vec::new());
        assert!(!has_focused_observations(&app));
        assert!(!super::super::chooser::surface_available(
            crate::app::DockSurface::Agents,
            &crate::work_context::PaneWorkContext::default(),
            true,
            has_focused_observations(&app),
        ));

        let terminal_id = app.workspaces[0]
            .focused_pane_id()
            .and_then(|pane_id| app.workspaces[0].terminal_id(pane_id))
            .expect("focused terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .claude_subagent_observations = Some(vec![observation(
            "a",
            None,
            "research",
            AgentState::Working,
            1,
        )]);

        assert!(has_focused_observations(&app));
        assert!(super::super::chooser::surface_available(
            crate::app::DockSurface::Agents,
            &crate::work_context::PaneWorkContext::default(),
            false,
            has_focused_observations(&app),
        ));
    }

    #[test]
    fn status_classes_reuse_sidebar_glyphs_and_colors() {
        let app = AppState::test_new();
        for (state, label) in [
            (AgentState::Working, "running"),
            (AgentState::Idle, "done"),
            (AgentState::Blocked, "blocked"),
        ] {
            let (glyph, actual_label, color) = status_class(state, &app);
            assert_eq!(
                glyph,
                crate::ui::sidebar::compact_dot_for_state(state, false, true, false, false, false)
            );
            assert_eq!(actual_label, label);
            assert_eq!(
                color,
                crate::ui::status::state_label_color(state, false, &app.palette)
            );
        }
    }

    #[test]
    fn selection_and_scroll_clamp_to_the_focused_tree() {
        let mut app = app_with(
            (0..6)
                .map(|index| {
                    observation(
                        &format!("a{index}"),
                        None,
                        &format!("agent {index}"),
                        AgentState::Working,
                        index,
                    )
                })
                .collect(),
        );
        app.view.dock_body_rect = Rect::new(40, 2, 30, 4);

        app.reconcile_dock_agents_selection();
        assert_eq!(app.dock_agents_selection.as_deref(), Some("a0"));
        for _ in 0..9 {
            app.move_dock_agents_selection(1);
        }
        assert_eq!(app.dock_agents_selection.as_deref(), Some("a5"));
        assert_eq!(app.dock_scroll, 3);
        app.scroll_dock_agents(99);
        assert_eq!(app.dock_scroll, 3);
        app.scroll_dock_agents(-99);
        assert_eq!(app.dock_scroll, 0);

        app.view.dock_agent_row_hit_areas = row_hit_areas(&app, app.view.dock_body_rect);
        assert!(app.click_dock_agent_row(42, 4));
        assert_eq!(app.dock_agents_selection.as_deref(), Some("a1"));
    }

    #[test]
    fn renderer_shows_hierarchy_status_and_age() {
        let mut app = app_with(vec![
            observation("a", None, "research", AgentState::Working, 1),
            observation("a1", Some("a"), "review", AgentState::Blocked, 2),
            observation("b", None, "tests", AgentState::Idle, 3),
        ]);
        app.dock_agents_selection = Some("a1".into());
        let backend = TestBackend::new(40, 6);
        let mut terminal = Terminal::new(backend).expect("test terminal");
        terminal
            .draw(|frame| {
                render_agents_at(
                    &app,
                    frame,
                    frame.area(),
                    SystemTime::UNIX_EPOCH + Duration::from_secs(10 * 60),
                )
            })
            .expect("render Agents surface");
        let buffer = terminal.backend().buffer();
        let rows = (0..6)
            .map(|row| {
                (0..40)
                    .map(|column| buffer[(column, row)].symbol())
                    .collect::<String>()
                    .trim_end()
                    .to_string()
            })
            .collect::<Vec<_>>();

        assert_eq!(rows[0], " subagents  3");
        assert!(rows[1].starts_with(" ● research"), "{:?}", rows);
        assert!(rows[1].ends_with("running  9m"), "{:?}", rows);
        assert!(rows[2].starts_with("   ○ review"), "{:?}", rows);
        assert!(rows[2].ends_with("blocked  8m"), "{:?}", rows);
        assert!(rows[3].ends_with("done  7m"), "{:?}", rows);
    }
}
