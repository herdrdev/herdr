pub mod components;
pub mod runs;
pub mod state;
pub mod tasks;

use crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
use ratatui::{
    buffer::Buffer,
    layout::Rect,
    style::{Modifier, Style},
};

use crate::app::state::Palette;
use components::{
    empty_state::render_empty_state,
    list_view::put_str,
    status_bar::render_status_bar,
};
#[allow(unused_imports)]
pub use runs::{ShepherdRunsAction, ShepherdRunsSectionState};
#[allow(unused_imports)]
pub use state::{
    ShepherdFocusedPanel, ShepherdSectionAction, ShepherdSectionHits, ShepherdSectionOutcome,
    ShepherdSectionState, ShepherdSectionTab,
};
#[allow(unused_imports)]
pub use tasks::{ShepherdTasksAction, ShepherdTasksSectionState};

pub fn contains_point(rect: Rect, point: (u16, u16)) -> bool {
    point.0 >= rect.x
        && point.0 < rect.x.saturating_add(rect.width)
        && point.1 >= rect.y
        && point.1 < rect.y.saturating_add(rect.height)
}

pub fn render_shepherd_section(
    buffer: &mut Buffer,
    area: Rect,
    state: &mut ShepherdSectionState,
    palette: &Palette,
) -> ShepherdSectionHits {
    let mut hits = ShepherdSectionHits::default();
    if area.width < 20 || area.height < 6 {
        return hits;
    }

    // 1. Top Bar: Title, Tabs and Close button
    let top_y = area.y;
    let mut tab_x = area.x;

    // Title label
    let title = " Shepherd ";
    let title_w = title.chars().count() as u16;
    put_str(
        buffer,
        tab_x,
        top_y,
        title_w,
        title,
        Style::default().fg(palette.accent).add_modifier(Modifier::BOLD),
    );
    tab_x += title_w + 1;

    // Tabs
    for &tab in &ShepherdSectionTab::ALL {
        let is_active = tab == state.active_tab;
        let tab_label = if area.width >= 90 {
            format!("[ {} ({}) ]", tab.label(), tab.shortcut())
        } else {
            format!("[{}]", tab.label())
        };
        let tab_w = tab_label.chars().count() as u16;
        if tab_x + tab_w < area.right().saturating_sub(18) {
            let tab_rect = Rect::new(tab_x, top_y, tab_w, 1);
            hits.tabs.push((tab_rect, tab));

            let tab_style = if is_active {
                Style::default().fg(palette.accent).add_modifier(Modifier::BOLD)
            } else {
                Style::default().fg(palette.overlay0)
            };
            put_str(buffer, tab_x, top_y, tab_w, &tab_label, tab_style);
            tab_x += tab_w + 1;
        }
    }

    // Close button (right-aligned)
    let close_text = "[ × Cerrar (Esc) ]";
    let close_w = close_text.chars().count() as u16;
    if area.right() >= close_w {
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
        ShepherdSectionTab::Agents => {
            hits.empty_state_hits = Some(render_empty_state(
                buffer,
                content_area,
                "PESTAÑA DE AGENTES (F10c)",
                "Próximamente en F10c: Formulario de lanzamiento en línea,\ngestión y franjas de confirmación de agentes integradas en este armazón.\nActualmente puedes gestionarlos en la barra lateral izquierda.",
                &[],
                palette,
            ));
        }
        ShepherdSectionTab::Patterns => {
            hits.empty_state_hits = Some(render_empty_state(
                buffer,
                content_area,
                "PESTAÑA DE PATRONES (F8c)",
                "Próximamente en F8c: Catálogo e inspección de topologías multi-agente\n(orchestrator-worker, blackboard, market) sobre este armazón.",
                &[],
                palette,
            ));
        }
        ShepherdSectionTab::Skills => {
            let actions = [("Abrir en Overlay temporal", Some("o"))];
            hits.empty_state_hits = Some(render_empty_state(
                buffer,
                content_area,
                "PESTAÑA DE SKILLS (F10c)",
                "Próximamente en F10c: Gestión de 3 capas (global/rol/agente)\ne inspección integrada en este armazón.\nPuedes abrir temporalmente el overlay de Skills con el botón inferior.",
                &actions,
                palette,
            ));
        }
        ShepherdSectionTab::Git => {
            let actions = [("Abrir en Overlay temporal", Some("o"))];
            hits.empty_state_hits = Some(render_empty_state(
                buffer,
                content_area,
                "PESTAÑA DE GIT (F10c)",
                "Próximamente en F10c: Gestión de worktrees y ramas en línea\nsobre este armazón.\nPuedes abrir temporalmente el overlay de Git con el botón inferior.",
                &actions,
                palette,
            ));
        }
        ShepherdSectionTab::Events => {
            hits.empty_state_hits = Some(render_empty_state(
                buffer,
                content_area,
                "PESTAÑA DE EVENTOS (F8d)",
                "Próximamente en F8d: Visualizador en tiempo real del bus JSONL\ncon controles de pausa, streaming y replay histórico.",
                &[],
                palette,
            ));
        }
    }

    // 3. Status Bar (Bottom line)
    let status_y = area.bottom().saturating_sub(1);
    let status_area = Rect::new(area.x, status_y, area.width, 1);

    let (summary, help_text) = match state.active_tab {
        ShepherdSectionTab::Tasks => {
            let total = state.tasks.tasks.len();
            let open_count = state
                .tasks
                .tasks
                .iter()
                .filter(|t| t.state.to_lowercase() == "open" || t.state.to_lowercase() == "active")
                .count();
            let sum = format!("{total} tareas ({open_count} abiertas)");
            let help = "↑/↓: lista | n: nueva | i: importar | a: archivar | e: editar | Esc: salir";
            (sum, help)
        }
        ShepherdSectionTab::Runs => {
            let total = state.runs.runs.len();
            let sum = format!("{total} ejecuciones");
            let help = "↑/↓: lista | l: iniciar run | c: cancelar | a: aprobar gate | d: rechazar | Esc: salir";
            (sum, help)
        }
        ShepherdSectionTab::Skills | ShepherdSectionTab::Git => {
            ("Vista provisional".into(), "o/Enter: abrir en overlay | Esc: salir")
        }
        _ => ("Próximamente".into(), "1-7: cambiar pestaña | Esc: salir"),
    };

    render_status_bar(
        buffer,
        status_area,
        &summary,
        help_text,
        state.status_message.as_deref(),
        palette,
    );

    hits
}

pub fn handle_shepherd_section_mouse(
    state: &mut ShepherdSectionState,
    hits: &ShepherdSectionHits,
    mouse: MouseEvent,
) -> ShepherdSectionOutcome {
    let point = (mouse.column, mouse.row);

    // Left click handling
    if mouse.kind == MouseEventKind::Down(MouseButton::Left) {
        // 1. Close button
        if let Some(close_rect) = hits.close_button {
            if contains_point(close_rect, point) {
                return ShepherdSectionOutcome::Close;
            }
        }

        // 2. Tabs
        for &(tab_rect, tab) in &hits.tabs {
            if contains_point(tab_rect, point) {
                state.select_tab(tab);
                return ShepherdSectionOutcome::Handled;
            }
        }

        // 3. Active tab mouse handling
        match state.active_tab {
            ShepherdSectionTab::Tasks => {
                if let Some(ref task_hits) = hits.tasks_hits {
                    // Filter tabs
                    for &(tab_rect, idx) in &task_hits.list_hits.filter_tabs {
                        if contains_point(tab_rect, point) {
                            state.tasks.filter_index = idx;
                            state.tasks.selected = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Action button in list header (+ Nueva)
                    if let Some(btn_rect) = task_hits.list_hits.action_button {
                        if contains_point(btn_rect, point) {
                            state.tasks.start_create_task();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // List rows
                    for &(row_rect, idx) in &task_hits.list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.tasks.selected = idx;
                            state.tasks.detail_scroll = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Inline form buttons
                    if let Some(ref form_hits) = task_hits.form_hits {
                        if let Some(sub_rect) = form_hits.submit_button {
                            if contains_point(sub_rect, point) {
                                if state.tasks.form.is_some() {
                                    if let Ok(Some(act)) = state.tasks.submit_create_task() {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Tasks(act),
                                        );
                                    }
                                } else if state.tasks.import_gh.is_some() {
                                    if let Ok(Some(act)) = state.tasks.submit_import_gh() {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Tasks(act),
                                        );
                                    }
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = form_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.tasks.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        for &(field_rect, field_idx) in &form_hits.field_rects {
                            if contains_point(field_rect, point) {
                                if let Some(ref mut f) = state.tasks.form {
                                    f.active_field = field_idx;
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Confirm archive stripe buttons
                    if let Some(ref stripe_hits) = task_hits.stripe_hits {
                        if let Some(conf_rect) = stripe_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.tasks.confirm_archive_task() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Tasks(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = stripe_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.tasks.confirm_archive = None;
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Detail view buttons
                    if let Some(ref detail_hits) = task_hits.detail_hits {
                        for &(btn_rect, action_idx) in &detail_hits.bottom_actions {
                            if contains_point(btn_rect, point) {
                                match action_idx {
                                    0 => {
                                        // Iniciar Run -> switch to Runs tab or start run
                                        state.select_tab(ShepherdSectionTab::Runs);
                                        if let Some(t) = state.tasks.selected_task() {
                                            state.runs.start_launch_run(t.id, false);
                                        }
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    1 => {
                                        // Archivar
                                        state.tasks.start_archive_task();
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    2 => {
                                        // Importar GitHub
                                        state.tasks.start_import_gh();
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    _ => {}
                                }
                            }
                        }
                        for &(btn_rect, _) in &detail_hits.header_actions {
                            if contains_point(btn_rect, point) {
                                if let Some(act) = state.tasks.open_editor_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Tasks(act),
                                    );
                                }
                            }
                        }
                    }

                    // Empty state CTA buttons
                    if let Some(ref empty_hits) = task_hits.empty_hits {
                        for &(btn_rect, idx) in &empty_hits.actions {
                            if contains_point(btn_rect, point) {
                                if idx == 0 {
                                    state.tasks.start_create_task();
                                } else {
                                    state.tasks.start_import_gh();
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
            ShepherdSectionTab::Runs => {
                if let Some(ref run_hits) = hits.runs_hits {
                    // Header action in list (+ Iniciar)
                    if let Some(btn_rect) = run_hits.list_hits.action_button {
                        if contains_point(btn_rect, point) {
                            let task_id = state
                                .runs
                                .selected_run()
                                .map(|r| r.task_id)
                                .unwrap_or(1);
                            state.runs.start_launch_run(task_id, false);
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // List rows
                    for &(row_rect, idx) in &run_hits.list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.runs.selected = idx;
                            state.runs.detail_scroll = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Launch form buttons
                    if let Some(ref form_hits) = run_hits.launch_form_hits {
                        if let Some(sub_rect) = form_hits.submit_button {
                            if contains_point(sub_rect, point) {
                                if let Some(act) = state.runs.submit_launch() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Runs(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = form_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.runs.launch_form = None;
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        for &(field_rect, field_idx) in &form_hits.field_rects {
                            if contains_point(field_rect, point) {
                                if let Some(ref mut f) = state.runs.launch_form {
                                    f.active_option = field_idx;
                                    f.toggle_active();
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Gate stripe buttons
                    if let Some(ref gate_hits) = run_hits.gate_stripe_hits {
                        if let Some(conf_rect) = gate_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.runs.approve_pending_gate() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Runs(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = gate_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                if let Some(act) = state.runs.reject_pending_gate() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Runs(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Cancel stripe buttons
                    if let Some(ref cancel_hits) = run_hits.cancel_stripe_hits {
                        if let Some(conf_rect) = cancel_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.runs.confirm_cancel_run() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Runs(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = cancel_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.runs.confirm_cancel = None;
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Detail view bottom actions
                    if let Some(ref detail_hits) = run_hits.detail_hits {
                        for &(btn_rect, action_idx) in &detail_hits.bottom_actions {
                            if contains_point(btn_rect, point) {
                                match action_idx {
                                    0 => {
                                        // Launch / Resume
                                        if let Some(r) = state.runs.selected_run() {
                                            state.runs.start_launch_run(r.task_id, r.has_saved_plan);
                                        }
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    1 => {
                                        // Cancel Run
                                        state.runs.start_cancel_run();
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    _ => {}
                                }
                            }
                        }
                    }

                    // Empty state CTA
                    if let Some(ref empty_hits) = run_hits.empty_hits {
                        for &(btn_rect, _) in &empty_hits.actions {
                            if contains_point(btn_rect, point) {
                                state.runs.start_launch_run(1, false);
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
            ShepherdSectionTab::Skills => {
                if let Some(ref empty_hits) = hits.empty_state_hits {
                    for &(btn_rect, _) in &empty_hits.actions {
                        if contains_point(btn_rect, point) {
                            return ShepherdSectionOutcome::Action(
                                ShepherdSectionAction::OpenLegacyOverlay(
                                    crate::shepherd::overlay::ShepherdTab::Skills,
                                ),
                            );
                        }
                    }
                }
            }
            ShepherdSectionTab::Git => {
                if let Some(ref empty_hits) = hits.empty_state_hits {
                    for &(btn_rect, _) in &empty_hits.actions {
                        if contains_point(btn_rect, point) {
                            return ShepherdSectionOutcome::Action(
                                ShepherdSectionAction::OpenLegacyOverlay(
                                    crate::shepherd::overlay::ShepherdTab::Git,
                                ),
                            );
                        }
                    }
                }
            }
            _ => {}
        }
    }

    // Scroll wheel handling
    if mouse.kind == MouseEventKind::ScrollUp {
        match state.active_tab {
            ShepherdSectionTab::Tasks => {
                state.tasks.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Runs => {
                state.runs.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            _ => {}
        }
    } else if mouse.kind == MouseEventKind::ScrollDown {
        match state.active_tab {
            ShepherdSectionTab::Tasks => {
                state.tasks.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Runs => {
                state.runs.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            _ => {}
        }
    }

    ShepherdSectionOutcome::Ignored
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
        let area = Rect::new(0, 0, 100, 25);
        let mut buffer = Buffer::empty(area);

        let mut state = ShepherdSectionState::default();
        assert_eq!(state.active_tab, ShepherdSectionTab::Tasks);

        let hits = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(!hits.tabs.is_empty());
        assert!(hits.close_button.is_some());
        assert!(hits.tasks_hits.is_some());

        // Title was rendered
        assert_eq!(buffer[(1, 0)].symbol(), "S");

        // Switch to Runs tab
        state.select_tab(ShepherdSectionTab::Runs);
        let hits_runs = render_shepherd_section(&mut buffer, area, &mut state, &palette);
        assert!(hits_runs.runs_hits.is_some());
    }

    #[test]
    fn test_shepherd_section_close_and_esc_state_machine() {
        let mut state = ShepherdSectionState::default();
        // Esc when clean closes section
        let outcome = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(outcome, ShepherdSectionOutcome::Close);

        // Esc when in create task form cancels form without closing section
        state.tasks.start_create_task();
        let outcome2 = state.handle_key(&crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Esc,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert_eq!(outcome2, ShepherdSectionOutcome::Handled);
        assert_eq!(state.tasks.form, None);
    }

    #[test]
    fn test_shepherd_section_mouse_click_handling() {
        use crossterm::event::KeyModifiers;

        let palette = test_palette();
        let area = Rect::new(0, 0, 100, 25);
        let mut buffer = Buffer::empty(area);
        let mut state = ShepherdSectionState::default();
        let hits = render_shepherd_section(&mut buffer, area, &mut state, &palette);

        // Click close button
        let close_rect = hits.close_button.unwrap();
        let mouse_close = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: close_rect.x,
            row: close_rect.y,
            modifiers: KeyModifiers::NONE,
        };
        let out = handle_shepherd_section_mouse(&mut state, &hits, mouse_close);
        assert_eq!(out, ShepherdSectionOutcome::Close);

        // Click Runs tab
        let runs_tab_rect = hits
            .tabs
            .iter()
            .find(|(_, t)| *t == ShepherdSectionTab::Runs)
            .unwrap()
            .0;
        let mouse_tab = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: runs_tab_rect.x,
            row: runs_tab_rect.y,
            modifiers: KeyModifiers::NONE,
        };
        let out = handle_shepherd_section_mouse(&mut state, &hits, mouse_tab);
        assert_eq!(out, ShepherdSectionOutcome::Handled);
        assert_eq!(state.active_tab, ShepherdSectionTab::Runs);
    }
}
