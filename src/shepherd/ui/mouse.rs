//! Mouse routing for the Shepherd section: tab bar, close button, per-tab hit rects and wheel.

use super::*;

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
            ShepherdSectionTab::Agents => {
                if let Some(ref agent_hits) = hits.agents_hits {
                    // Snapshot timeline owns every click while it is open
                    if state.agents.snapshots.open {
                        if let Some(ref sh) = agent_hits.snapshots {
                            if let Some(o) = snapshots_mouse::click(state, sh, point) {
                                return o;
                            }
                        }
                        return ShepherdSectionOutcome::Ignored;
                    }

                    // Header action (+ Nuevo)
                    if let Some(btn_rect) = agent_hits.list_hits.action_button {
                        if contains_point(btn_rect, point) {
                            state.agents.start_spawn();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // List rows
                    for &(row_rect, idx) in &agent_hits.list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.agents.selected = idx;
                            state.agents.detail_scroll = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Spawn form buttons
                    if let Some(ref form_hits) = agent_hits.form_hits {
                        if let Some(sub_rect) = form_hits.submit_button {
                            if contains_point(sub_rect, point) {
                                if let Ok(Some(act)) = state.agents.submit_spawn() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Agents(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = form_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.agents.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        for &(field_rect, field_idx) in &form_hits.field_rects {
                            if contains_point(field_rect, point) {
                                if let Some(ref mut f) = state.agents.spawn_form {
                                    f.active_field = field_idx;
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Stripe buttons
                    if let Some(ref stripe_hits) = agent_hits.stripe_hits {
                        if let Some(conf_rect) = stripe_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.agents.confirm_kill_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Agents(act),
                                    );
                                } else if let Some(act) = state.agents.confirm_clean_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Agents(act),
                                    );
                                } else if let Some(act) = state.agents.confirm_clean_all_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Agents(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = stripe_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.agents.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Detail view actions
                    if let Some(ref detail_hits) = agent_hits.detail_hits {
                        for &(btn_rect, action_idx) in &detail_hits.bottom_actions {
                            if contains_point(btn_rect, point) {
                                match action_idx {
                                    0 => {
                                        if let Some(act) = state.agents.focus_selected_agent() {
                                            return ShepherdSectionOutcome::Action(
                                                ShepherdSectionAction::Agents(act),
                                            );
                                        }
                                    }
                                    1 => state.agents.start_kill(),
                                    2 => state.agents.start_clean(),
                                    3 => {
                                        if let Some(act) = state.agents.open_timeline() {
                                            return ShepherdSectionOutcome::Action(
                                                ShepherdSectionAction::Agents(act),
                                            );
                                        }
                                    }
                                    4 => state.agents.start_clean_all(),
                                    _ => {}
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        for &(btn_rect, action_idx) in &detail_hits.header_actions {
                            if contains_point(btn_rect, point) {
                                if action_idx == 0 {
                                    if let Some(act) = state.agents.focus_selected_agent() {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Agents(act),
                                        );
                                    }
                                } else {
                                    state.agents.start_spawn();
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Empty state CTA
                    if let Some(ref empty_hits) = agent_hits.empty_hits {
                        for &(btn_rect, idx) in &empty_hits.actions {
                            if contains_point(btn_rect, point) {
                                match idx {
                                    1 => {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Agents(
                                                ShepherdAgentsAction::Refresh,
                                            ),
                                        )
                                    }
                                    2 => {
                                        if let Some(act) = state.agents.open_timeline() {
                                            return ShepherdSectionOutcome::Action(
                                                ShepherdSectionAction::Agents(act),
                                            );
                                        }
                                    }
                                    _ => state.agents.start_spawn(),
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
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
                                        state.select_tab(ShepherdSectionTab::Runs);
                                        if let Some(t) = state.tasks.selected_task() {
                                            state.runs.start_launch_run(t.id, false);
                                        }
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    1 => {
                                        state.tasks.start_archive_task();
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    2 => {
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
                            let task_id = state.runs.selected_run().map(|r| r.task_id).unwrap_or(1);
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
                                        if let Some(r) = state.runs.selected_run() {
                                            state
                                                .runs
                                                .start_launch_run(r.task_id, r.has_saved_plan);
                                        }
                                        return ShepherdSectionOutcome::Handled;
                                    }
                                    1 => {
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
            ShepherdSectionTab::Patterns => {
                if let Some(ref pattern_hits) = hits.patterns_hits {
                    let (handled, act) =
                        state
                            .patterns
                            .handle_mouse(mouse.column, mouse.row, pattern_hits);
                    if let Some(a) = act {
                        return ShepherdSectionOutcome::Action(ShepherdSectionAction::Patterns(a));
                    } else if handled {
                        return ShepherdSectionOutcome::Handled;
                    }
                }
            }
            ShepherdSectionTab::Skills => {
                if let Some(ref skill_hits) = hits.skills_hits {
                    // Filter / mode tabs ([Por Agente], [Catálogo])
                    for &(tab_rect, idx) in &skill_hits.list_hits.filter_tabs {
                        if contains_point(tab_rect, point) {
                            let new_mode = if idx == 0 {
                                skills::SkillsViewMode::AgentView
                            } else {
                                skills::SkillsViewMode::CatalogView
                            };
                            if state.skills.mode != new_mode {
                                state.skills.toggle_mode();
                            }
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Header action in list (+ Nueva)
                    if let Some(btn_rect) = skill_hits.list_hits.action_button {
                        if contains_point(btn_rect, point) {
                            state.skills.start_create_skill();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // List rows
                    for &(row_rect, idx) in &skill_hits.list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.skills.selected = idx;
                            state.skills.detail_scroll = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Form buttons
                    if let Some(ref form_hits) = skill_hits.form_hits {
                        if let Some(sub_rect) = form_hits.submit_button {
                            if contains_point(sub_rect, point) {
                                if let Ok(Some(act)) = state.skills.submit_create_skill() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Skills(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = form_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.skills.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Confirm stripes
                    if let Some(ref stripe_hits) = skill_hits.stripe_hits {
                        if let Some(conf_rect) = stripe_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.skills.confirm_restore_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Skills(act),
                                    );
                                } else if let Some(act) = state.skills.confirm_delete_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Skills(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = stripe_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.skills.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Detail view actions
                    if let Some(ref detail_hits) = skill_hits.detail_hits {
                        for &(btn_rect, action_idx) in &detail_hits.bottom_actions {
                            if contains_point(btn_rect, point) {
                                match state.skills.mode {
                                    skills::SkillsViewMode::AgentView => {
                                        if action_idx == 0 {
                                            if let Some(act) = state.skills.toggle_agent_skill() {
                                                return ShepherdSectionOutcome::Action(
                                                    ShepherdSectionAction::Skills(act),
                                                );
                                            }
                                        } else {
                                            state.skills.start_restore_skill();
                                        }
                                    }
                                    skills::SkillsViewMode::CatalogView => {
                                        if action_idx == 0 {
                                            if let Some(act) = state.skills.toggle_catalog_global()
                                            {
                                                return ShepherdSectionOutcome::Action(
                                                    ShepherdSectionAction::Skills(act),
                                                );
                                            }
                                        } else {
                                            state.skills.start_delete_skill();
                                        }
                                    }
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Empty state CTA
                    if let Some(ref empty_hits) = skill_hits.empty_hits {
                        for &(btn_rect, _) in &empty_hits.actions {
                            if contains_point(btn_rect, point) {
                                match state.skills.mode {
                                    skills::SkillsViewMode::AgentView => state.skills.toggle_mode(),
                                    skills::SkillsViewMode::CatalogView => {
                                        state.skills.start_create_skill()
                                    }
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
            ShepherdSectionTab::Git => {
                if let Some(ref git_hits) = hits.git_hits {
                    if git_hits
                        .timeline_button
                        .is_some_and(|r| contains_point(r, point))
                    {
                        return state.open_timeline_from_git();
                    }
                    // Header action (+ Worktree)
                    if let Some(btn_rect) = git_hits.list_hits.action_button {
                        if contains_point(btn_rect, point) {
                            state.git.start_create_worktree();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // List rows
                    for &(row_rect, idx) in &git_hits.list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.git.selected = idx;
                            state.git.detail_scroll = 0;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Form buttons
                    if let Some(ref form_hits) = git_hits.form_hits {
                        if let Some(sub_rect) = form_hits.submit_button {
                            if contains_point(sub_rect, point) {
                                if state.git.worktree_form.is_some() {
                                    if let Ok(Some(act)) = state.git.submit_create_worktree() {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Git(act),
                                        );
                                    }
                                } else if state.git.branch_form.is_some() {
                                    if let Ok(Some(act)) = state.git.submit_create_branch() {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Git(act),
                                        );
                                    }
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = form_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.git.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Confirm stripes
                    if let Some(ref stripe_hits) = git_hits.stripe_hits {
                        if let Some(conf_rect) = stripe_hits.confirm_button {
                            if contains_point(conf_rect, point) {
                                if let Some(act) = state.git.confirm_publish_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Git(act),
                                    );
                                } else if let Some(act) = state.git.confirm_remove_action() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Git(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = stripe_hits.cancel_button {
                            if contains_point(can_rect, point) {
                                state.git.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Detail view actions
                    if let Some(ref detail_hits) = git_hits.detail_hits {
                        for &(btn_rect, action_idx) in &detail_hits.bottom_actions {
                            if contains_point(btn_rect, point) {
                                match action_idx {
                                    0 => state.git.start_create_branch(),
                                    1 => state.git.start_publish_branch(),
                                    2 => {
                                        if let Some(r) = state.git.selected_row() {
                                            return ShepherdSectionOutcome::Action(
                                                ShepherdSectionAction::Git(
                                                    ShepherdGitAction::CopyOrShowPath {
                                                        path: std::path::PathBuf::from(&r.path),
                                                    },
                                                ),
                                            );
                                        }
                                    }
                                    3 => {
                                        return ShepherdSectionOutcome::Action(
                                            ShepherdSectionAction::Git(ShepherdGitAction::Refresh),
                                        );
                                    }
                                    4 => state.git.start_remove_worktree(),
                                    _ => {}
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        for &(btn_rect, action_idx) in &detail_hits.header_actions {
                            if contains_point(btn_rect, point) {
                                if action_idx == 0 {
                                    state.git.start_create_worktree();
                                } else {
                                    state.git.start_create_branch();
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }

                    // Empty state CTA
                    if let Some(ref empty_hits) = git_hits.empty_hits {
                        for &(btn_rect, _) in &empty_hits.actions {
                            if contains_point(btn_rect, point) {
                                state.git.start_create_worktree();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
            ShepherdSectionTab::Events => {
                if let Some(ref ev_hits) = hits.events_hits {
                    // Subtabs
                    for &(tab_rect, sub_tab) in &ev_hits.subtab_rects {
                        if contains_point(tab_rect, point) {
                            state.events.active_subtab = sub_tab;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Topic list rows
                    for &(row_rect, idx) in &ev_hits.topic_list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.events.selected_topic = idx;
                            state.events.trigger_topic_load();
                            if let Some(act) = state.events.last_action.take() {
                                return ShepherdSectionOutcome::Action(
                                    ShepherdSectionAction::Events(act),
                                );
                            }
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Run list rows
                    for &(row_rect, idx) in &ev_hits.run_list_hits.rows {
                        if contains_point(row_rect, point) {
                            state.events.selected_run = idx;
                            state.events.trigger_run_load();
                            if let Some(act) = state.events.last_action.take() {
                                return ShepherdSectionOutcome::Action(
                                    ShepherdSectionAction::Events(act),
                                );
                            }
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Offset / filter inputs: click to edit
                    if let Some(r) = ev_hits.replay_input_rect {
                        if contains_point(r, point) {
                            state.events.editing_agent_filter = false;
                            state.events.editing_replay_offset = true;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }
                    if let Some(r) = ev_hits.filter_input_rect {
                        if contains_point(r, point) {
                            state.events.editing_replay_offset = false;
                            state.events.editing_agent_filter = true;
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Empty state CTA: go to the Patterns tab
                    if let Some(ref empty_hits) = ev_hits.empty_hits {
                        if empty_hits
                            .actions
                            .iter()
                            .any(|&(r, _)| contains_point(r, point))
                        {
                            state.select_tab(ShepherdSectionTab::Patterns);
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Pause view button
                    if let Some(btn_rect) = ev_hits.pause_view_button {
                        if contains_point(btn_rect, point) {
                            state.events.toggle_pause_view();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Replay button
                    if let Some(btn_rect) = ev_hits.replay_button {
                        if contains_point(btn_rect, point) {
                            state.events.trigger_replay();
                            if let Some(act) = state.events.last_action.take() {
                                return ShepherdSectionOutcome::Action(
                                    ShepherdSectionAction::Events(act),
                                );
                            }
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Pause run button
                    if let Some(btn_rect) = ev_hits.pause_run_button {
                        if contains_point(btn_rect, point) {
                            state.events.toggle_pause_selected_run();
                            if let Some(act) = state.events.last_action.take() {
                                return ShepherdSectionOutcome::Action(
                                    ShepherdSectionAction::Events(act),
                                );
                            }
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Stop run button
                    if let Some(btn_rect) = ev_hits.stop_run_button {
                        if contains_point(btn_rect, point) {
                            state.events.start_stop_run();
                            return ShepherdSectionOutcome::Handled;
                        }
                    }

                    // Stripe confirm/cancel buttons
                    if let Some(ref sh) = ev_hits.confirm_stripe_hits {
                        if let Some(conf_rect) = sh.confirm_button {
                            if contains_point(conf_rect, point) {
                                state.events.confirm_stop();
                                if let Some(act) = state.events.last_action.take() {
                                    return ShepherdSectionOutcome::Action(
                                        ShepherdSectionAction::Events(act),
                                    );
                                }
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                        if let Some(can_rect) = sh.cancel_button {
                            if contains_point(can_rect, point) {
                                state.events.cancel_sub_action();
                                return ShepherdSectionOutcome::Handled;
                            }
                        }
                    }
                }
            }
        }
    }

    // Snapshot timeline: wheel scrolls the diff over the detail, else moves the selection.
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && state.active_tab == ShepherdSectionTab::Agents
        && state.agents.snapshots.open
    {
        if let Some(sh) = hits.agents_hits.as_ref().and_then(|h| h.snapshots.as_ref()) {
            return snapshots_mouse::wheel(
                state,
                sh,
                point,
                mouse.kind == MouseEventKind::ScrollUp,
            );
        }
        return ShepherdSectionOutcome::Ignored;
    }

    // Wheel over the event message table scrolls the stream, not the topic list.
    if matches!(
        mouse.kind,
        MouseEventKind::ScrollUp | MouseEventKind::ScrollDown
    ) && state.active_tab == ShepherdSectionTab::Events
    {
        let over_messages = hits
            .events_hits
            .as_ref()
            .and_then(|h| h.messages_rect)
            .is_some_and(|r| contains_point(r, point));
        if over_messages {
            let up = mouse.kind == MouseEventKind::ScrollUp;
            state.events.scroll_messages(up);
            return ShepherdSectionOutcome::Handled;
        }
    }

    // Scroll wheel handling
    if mouse.kind == MouseEventKind::ScrollUp {
        match state.active_tab {
            ShepherdSectionTab::Agents => {
                state.agents.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Tasks => {
                state.tasks.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Runs => {
                state.runs.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Patterns => {
                state.patterns.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Skills => {
                state.skills.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Git => {
                state.git.select_prev();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Events => {
                state.events.select_prev_item();
                return ShepherdSectionOutcome::Handled;
            }
        }
    } else if mouse.kind == MouseEventKind::ScrollDown {
        match state.active_tab {
            ShepherdSectionTab::Agents => {
                state.agents.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Tasks => {
                state.tasks.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Runs => {
                state.runs.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Patterns => {
                state.patterns.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Skills => {
                state.skills.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Git => {
                state.git.select_next();
                return ShepherdSectionOutcome::Handled;
            }
            ShepherdSectionTab::Events => {
                state.events.select_next_item();
                return ShepherdSectionOutcome::Handled;
            }
        }
    }

    ShepherdSectionOutcome::Ignored
}
