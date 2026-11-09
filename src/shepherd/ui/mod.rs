pub mod agents;
pub mod components;
pub mod events;
pub mod git;
#[cfg(test)]
mod hit_tests;
pub mod mouse;
pub mod patterns;
pub mod runs;
pub mod skills;
pub mod snapshots;
#[cfg(test)]
mod snapshots_links_tests;
mod snapshots_mouse;
#[cfg(test)]
mod snapshots_tests;
pub mod snapshots_view;
pub mod state;
pub mod tasks;

use components::list_view::text_width;
use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
#[allow(unused_imports)]
pub use agents::{ShepherdAgentsAction, ShepherdAgentsHits, ShepherdAgentsSectionState};
use components::{list_view::put_str, status_bar::render_status_bar};
#[allow(unused_imports)]
pub use events::{ShepherdEventsAction, ShepherdEventsHits, ShepherdEventsSectionState};
#[allow(unused_imports)]
pub use git::{ShepherdGitAction, ShepherdGitHits, ShepherdGitSectionState};
pub use mouse::handle_shepherd_section_mouse;
#[allow(unused_imports)]
pub use patterns::{ShepherdPatternsAction, ShepherdPatternsHits, ShepherdPatternsSectionState};
#[allow(unused_imports)]
pub use runs::{ShepherdRunsAction, ShepherdRunsHits, ShepherdRunsSectionState};
#[allow(unused_imports)]
pub use skills::{ShepherdSkillsAction, ShepherdSkillsHits, ShepherdSkillsSectionState};
#[allow(unused_imports)]
pub use state::{
    ShepherdFocusedPanel, ShepherdSectionAction, ShepherdSectionHits, ShepherdSectionOutcome,
    ShepherdSectionState, ShepherdSectionTab,
};
#[allow(unused_imports)]
pub use tasks::{ShepherdTasksAction, ShepherdTasksHits, ShepherdTasksSectionState};

pub fn contains_point(rect: Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x
        && point.0 < rect.x.saturating_add(rect.width)
        && point.1 >= rect.y
        && point.1 < rect.y.saturating_add(rect.height)
}

const MIN_WIDTH: u16 = 30;
const MIN_HEIGHT: u16 = 8;

fn render_too_small(buffer: &mut Buffer, area: Rect, palette: &Palette) {
    let msg = format!(
        "Enlarge the terminal to use Shepherd (minimum {MIN_WIDTH}x{MIN_HEIGHT}, now {}x{}). Esc: quit",
        area.width, area.height
    );
    for (i, line) in components::list_view::wrap_text(&msg, area.width)
        .iter()
        .take(area.height as usize)
        .enumerate()
    {
        put_str(
            buffer,
            area.x,
            area.y + i as u16,
            area.width,
            line,
            Style::default().fg(palette.yellow),
        );
    }
}

pub fn render_shepherd_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &mut ShepherdSectionState,
    palette: &Palette,
) -> ShepherdSectionHits {
    let mut hits = ShepherdSectionHits::default();
    if area.width < MIN_WIDTH || area.height < MIN_HEIGHT {
        render_too_small(buffer, area, palette);
        return hits;
    }

    // 1. Top Bar: Title, Tabs and Close button
    let top_y = area.y;
    let mut tab_x = area.x;

    // Title label
    let title = " Shepherd ";
    let title_w = text_width(title);
    put_str(
        buffer,
        tab_x,
        top_y,
        title_w,
        title,
        Style::default()
            .fg(palette.accent)
            .add_modifier(Modifier::BOLD),
    );
    tab_x += title_w + 1;

    // Close button (right-aligned), shortened when the bar is tight
    let close_text = if area.width >= 60 {
        "[ × Close (Esc) ]"
    } else {
        "[×]"
    };
    let close_w = text_width(close_text);
    let close_x = area.right().saturating_sub(close_w);
    let close_rect = Rect::new(close_x, top_y, close_w, 1);
    hits.close_button = Some(close_rect);
    put_str(
        buffer,
        close_x,
        top_y,
        close_w,
        close_text,
        Style::default().fg(palette.overlay0),
    );

    // Tabs: richest label style in which all of them fit
    let avail = close_x.saturating_sub(tab_x + 1);
    let label_for = |tab: ShepherdSectionTab, style: u8| match style {
        0 => format!("[ {} ({}) ]", tab.label(), tab.shortcut()),
        1 => format!("[{}]", tab.label()),
        _ => format!("[{}]", tab.shortcut()),
    };
    let style_idx = (0u8..=2)
        .find(|&st| {
            let total: u16 = ShepherdSectionTab::ALL
                .iter()
                .map(|&t| text_width(label_for(t, st)) + 1)
                .sum();
            total <= avail + 1
        })
        .unwrap_or(2);
    for &tab in &ShepherdSectionTab::ALL {
        let is_active = tab == state.active_tab;
        let tab_label = label_for(tab, style_idx);
        let tab_w = text_width(&tab_label);
        if tab_x + tab_w <= close_x {
            let tab_rect = Rect::new(tab_x, top_y, tab_w, 1);
            hits.tabs.push((tab_rect, tab));

            let tab_style = if is_active {
                Style::default()
                    .fg(palette.accent)
                    .add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.overlay0)
            };
            put_str(buffer, tab_x, top_y, tab_w, &tab_label, tab_style);
            tab_x += tab_w + 1;
        }
    }

    // Horizontal top divider
    let div_y = top_y + 1;
    let sep = "─".repeat(area.width as usize);
    put_str(
        buffer,
        area.x,
        div_y,
        area.width,
        &sep,
        Style::default().fg(palette.surface_dim),
    );

    // 2. Central Content Area
    let content_y = div_y + 1;
    let status_bar_h: u16 = 1;
    let content_h = area.bottom().saturating_sub(content_y + status_bar_h);
    let content_area = Rect::new(area.x, content_y, area.width, content_h);

    match state.active_tab {
        ShepherdSectionTab::Agents => {
            hits.agents_hits = Some(agents::render_agents_section(
                buffer,
                content_area,
                &state.agents,
                palette,
            ));
        }
        ShepherdSectionTab::Tasks => {
            hits.tasks_hits = Some(tasks::render_tasks_section(
                buffer,
                content_area,
                &state.tasks,
                palette,
            ));
        }
        ShepherdSectionTab::Runs => {
            hits.runs_hits = Some(runs::render_runs_section(
                buffer,
                content_area,
                &state.runs,
                palette,
            ));
        }
        ShepherdSectionTab::Patterns => {
            hits.patterns_hits = Some(patterns::render_patterns_section(
                buffer,
                content_area,
                &state.patterns,
                palette,
            ));
        }
        ShepherdSectionTab::Skills => {
            hits.skills_hits = Some(skills::render_skills_section(
                buffer,
                content_area,
                &state.skills,
                palette,
            ));
        }
        ShepherdSectionTab::Git => {
            hits.git_hits = Some(git::render_git_section_with_agents(
                buffer,
                content_area,
                &state.git,
                &state.agents.agents,
                palette,
            ));
        }
        ShepherdSectionTab::Events => {
            hits.events_hits = Some(events::render_events_section(
                buffer,
                content_area,
                &mut state.events,
                palette,
            ));
        }
    }

    // 3. Status Bar (Bottom line)
    let status_y = area.bottom().saturating_sub(1);
    let status_area = Rect::new(area.x, status_y, area.width, 1);

    let (summary, help_text) = match state.active_tab {
        ShepherdSectionTab::Agents => {
            let total = state.agents.agents.len();
            let needs_you = state.agents.needs_you_count();
            let sum = if needs_you > 0 {
                format!("{total} agents ({needs_you} NEED ATTENTION)")
            } else {
                format!("{total} agents registered")
            };
            if state.agents.snapshots.open {
                let snaps = &state.agents.snapshots;
                let sum = match snaps.source {
                    snapshots::SnapshotSource::Agent => format!(
                        "Snapshots of {} ({})",
                        snaps.agent_id,
                        snaps.agent_snaps.len()
                    ),
                    snapshots::SnapshotSource::Finished => {
                        format!(
                            "Snapshots of finished agents ({})",
                            snaps.finished_snaps.len()
                        )
                    }
                };
                let help = "↑/↓: list | c: capture | d/D: diff | f: file | R: all | x: delete | p: prune | Tab: finished | Esc: back";
                (sum, help)
            } else {
                let help = "↑/↓: list | Enter: terminal | s: new | t: snapshots | K: kill | x: clean | r: refresh | Esc: quit";
                (sum, help)
            }
        }
        ShepherdSectionTab::Tasks => {
            let total = state.tasks.tasks.len();
            let open_count = state
                .tasks
                .tasks
                .iter()
                .filter(|t| t.state.to_lowercase() == "open" || t.state.to_lowercase() == "active")
                .count();
            let sum = format!("{total} tareas ({open_count} abiertas)");
            let help = "↑/↓: list | n: new | i: import | a: archive | e: edit | Esc: quit";
            (sum, help)
        }
        ShepherdSectionTab::Runs => {
            let total = state.runs.runs.len();
            let sum = format!("{total} runs");
            let help =
                "↑/↓: list | l: start run | c: cancel | a: approve gate | d: reject | Esc: quit";
            (sum, help)
        }
        ShepherdSectionTab::Patterns => {
            let total = state.patterns.patterns.len();
            let sum = format!("{total} patrones disponibles");
            let help =
                "↑/↓: list | n: new | e: edit | c: duplicate | v: validate | d: delete | Esc: quit";
            (sum, help)
        }
        ShepherdSectionTab::Skills => {
            let sum = match state.skills.mode {
                skills::SkillsViewMode::AgentView => {
                    format!("Agent skills ({})", state.skills.agent_skills.len())
                }
                skills::SkillsViewMode::CatalogView => {
                    format!("Skill catalog ({})", state.skills.catalog_skills.len())
                }
            };
            let help = match state.skills.mode {
                skills::SkillsViewMode::AgentView => {
                    "c: catalog | Space: enable/disable | r: restore | Esc: quit"
                }
                skills::SkillsViewMode::CatalogView => {
                    "c: by agent | n: new | g: global | d: delete | Esc: quit"
                }
            };
            (sum, help)
        }
        ShepherdSectionTab::Git => {
            let total = state.git.rows.len();
            let dirty = state.git.rows.iter().filter(|r| !r.is_clean).count();
            let sum = format!("{total} worktrees ({dirty} modificados)");
            let help = "↑/↓: list | w: new worktree | b: new branch | p: publish | t: snapshots | x: delete | c: copy path | Esc: quit";
            (sum, help)
        }
        ShepherdSectionTab::Events => {
            let sum = match state.events.active_subtab {
                events::ShepherdEventsSubTab::Topics => {
                    let paused_str = if state.events.view_paused {
                        " (PAUSADO)"
                    } else {
                        ""
                    };
                    format!(
                        "Topics and Streaming{paused_str} ({} msgs)",
                        state.events.messages.len()
                    )
                }
                events::ShepherdEventsSubTab::PatternRuns => {
                    format!("Pattern runs ({})", state.events.runs.len())
                }
            };
            let help = match state.events.active_subtab {
                events::ShepherdEventsSubTab::Topics => {
                    "Tab: switch view | Space: pause/resume | r: replay | /: filter | Esc: quit"
                }
                events::ShepherdEventsSubTab::PatternRuns => {
                    "Tab: switch view | p: pause/resume | K: stop | ↑/↓: list | Esc: quit"
                }
            };
            (sum, help)
        }
    };

    let help_text = if state.has_active_sub_modal_or_stripe() {
        "Tab: field | Enter: confirm | Esc: cancel"
    } else {
        help_text
    };
    let notice = state.status_message.as_deref();

    render_status_bar(buffer, status_area, &summary, help_text, notice, palette);

    hits
}

#[cfg(test)]
mod tests {
    use super::*;

    fn test_palette() -> Palette {
        Palette::catppuccin()
    }

    #[test]
    fn test_shepherd_section_pure_render_and_tab_switching() {
        let palette = test_palette();
        let area = Rect::new(0, 0, 140, 25);
        let mut buffer = Buffer::empty(area);
        let mut state = ShepherdSectionState::default();

        let hits = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert_eq!(hits.tabs.len(), 7);
        assert!(hits.close_button.is_some());
        assert!(hits.agents_hits.is_some());

        // Switch to Tasks
        state.select_tab(ShepherdSectionTab::Tasks);
        let hits2 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits2.tasks_hits.is_some());

        // Switch to Runs
        state.select_tab(ShepherdSectionTab::Runs);
        let hits3 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits3.runs_hits.is_some());

        // Switch to Skills
        state.select_tab(ShepherdSectionTab::Skills);
        let hits4 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits4.skills_hits.is_some());

        // Switch to Git
        state.select_tab(ShepherdSectionTab::Git);
        let hits5 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits5.git_hits.is_some());

        // Switch to Patterns (gallery view)
        state.select_tab(ShepherdSectionTab::Patterns);
        let hits6 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits6.patterns_hits.is_some());

        // Switch to Events
        state.select_tab(ShepherdSectionTab::Events);
        let hits7 = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits7.events_hits.is_some());
    }

    #[test]
    fn test_shepherd_section_close_and_esc_state_machine() {
        let mut state = ShepherdSectionState::default();
        state.select_tab(ShepherdSectionTab::Agents);

        // 1. Esc with sub-action: cancels sub-action, handled
        state.agents.start_spawn();
        assert!(state.has_active_sub_modal_or_stripe());
        let outcome = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(outcome, ShepherdSectionOutcome::Handled);
        assert!(!state.has_active_sub_modal_or_stripe());

        // 2. Esc without sub-action: closes section
        let outcome2 = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(outcome2, ShepherdSectionOutcome::Close);
    }
}
