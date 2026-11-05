//! State machine, user actions, and interactive hit targets for patterns.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use shepherd_core::patterns::{
    get_builtin_pattern, validate, Pattern, PatternDetail, PatternKind, PatternSource,
    PatternSummary,
};

use super::editor::{EditorSection, PatternEditorHits, PatternEditorState};
use crate::shepherd::ui::components::{
    confirm_stripe::ConfirmStripeHits, detail_view::DetailViewHits, empty_state::EmptyStateHits,
    inline_form::InlineFormHits, list_view::ListViewHits,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ShepherdPatternsAction {
    List,
    Get {
        name: String,
    },
    Save {
        pattern: Pattern,
        global: bool,
    },
    Delete {
        name: String,
    },
    Validate {
        name: Option<String>,
        pattern: Option<Pattern>,
    },
    RunPattern {
        pattern: String,
        task: Option<String>,
        worktree: Option<String>,
    },
}

#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct ShepherdPatternsHits {
    pub list_hits: ListViewHits,
    pub detail_hits: Option<DetailViewHits>,
    pub editor_hits: Option<PatternEditorHits>,
    pub stripe_hits: Option<ConfirmStripeHits>,
    pub empty_hits: Option<EmptyStateHits>,
    pub new_from_template_hits: Option<InlineFormHits>,
    pub duplicate_hits: Option<InlineFormHits>,
    pub run_form_hits: Option<InlineFormHits>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum PatternSubAction {
    NewFromTemplate(NewFromTemplateState),
    Duplicate(DuplicatePatternState),
    ConfirmDelete { name: String, source: PatternSource },
    Edit(PatternEditorState),
    Run(PatternRunFormState),
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct PatternRunFormState {
    pub pattern_name: String,
    pub task: String,
    pub worktree: String,
    pub active_field: usize, // 0: task, 1: worktree
}

impl PatternRunFormState {
    pub fn new(pattern_name: String) -> Self {
        Self {
            pattern_name,
            task: String::new(),
            worktree: String::new(),
            active_field: 0,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct NewFromTemplateState {
    pub kind_index: usize, // 0..3
    pub name: String,
    pub is_global: bool,
    pub active_field: usize, // 0: kind, 1: name, 2: target
}

impl Default for NewFromTemplateState {
    fn default() -> Self {
        Self {
            kind_index: 0,
            name: "mi-orquestador".into(),
            is_global: false,
            active_field: 1,
        }
    }
}

impl NewFromTemplateState {
    pub fn selected_kind(&self) -> PatternKind {
        match self.kind_index {
            0 => PatternKind::OrchestratorWorker,
            1 => PatternKind::Hierarchical,
            2 => PatternKind::Blackboard,
            _ => PatternKind::Market,
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct DuplicatePatternState {
    pub source_name: String,
    pub name: String,
    pub is_global: bool,
    pub active_field: usize, // 0: name, 1: target
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ShepherdPatternsSectionState {
    pub selected: usize,
    pub filter_index: usize, // 0: Todos, 1: Proyecto, 2: Global, 3: Plantillas
    pub list_scroll: usize,
    pub detail_scroll: usize,
    pub patterns: Vec<PatternSummary>,
    pub current_detail: Option<PatternDetail>,
    pub sub_action: Option<PatternSubAction>,
    pub last_action: Option<ShepherdPatternsAction>,
    pub error: Option<String>,
}

impl Default for ShepherdPatternsSectionState {
    fn default() -> Self {
        let mut s = Self {
            selected: 0,
            filter_index: 0,
            list_scroll: 0,
            detail_scroll: 0,
            patterns: Vec::new(),
            current_detail: None,
            sub_action: None,
            last_action: None,
            error: None,
        };
        s.load_builtin_defaults();
        s
    }
}

impl ShepherdPatternsSectionState {
    pub fn load_builtin_defaults(&mut self) {
        if self.patterns.is_empty() {
            self.patterns = vec![
                PatternSummary {
                    name: "orchestrator-worker".into(),
                    kind: PatternKind::OrchestratorWorker,
                    description: "Central orchestrator dispatches tasks to a worker pool over partitioned topics.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 4,
                    topics_count: 2,
                    path: None,
                },
                PatternSummary {
                    name: "hierarchical".into(),
                    kind: PatternKind::Hierarchical,
                    description: "Multi-tier command hierarchy: root director dispatches to managers, who coordinate specialists.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 7,
                    topics_count: 4,
                    path: None,
                },
                PatternSummary {
                    name: "blackboard".into(),
                    kind: PatternKind::Blackboard,
                    description: "Shared knowledge base where specialist agents read hypotheses and post partial discoveries.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 3,
                    topics_count: 1,
                    path: None,
                },
                PatternSummary {
                    name: "market".into(),
                    kind: PatternKind::Market,
                    description: "Competitive market: requesters post tasks to ask topic, solvers bid, market-maker matches.".into(),
                    source: PatternSource::Builtin,
                    shadowed: false,
                    agents_count: 6,
                    topics_count: 3,
                    path: None,
                },
            ];
            self.update_current_detail_from_selection();
        }
    }

    pub fn filtered_indices(&self) -> Vec<usize> {
        self.patterns
            .iter()
            .enumerate()
            .filter(|(_, p)| match self.filter_index {
                1 => p.source == PatternSource::Project,
                2 => p.source == PatternSource::Global,
                3 => p.source == PatternSource::Builtin,
                _ => true,
            })
            .map(|(i, _)| i)
            .collect()
    }

    pub fn selected_summary(&self) -> Option<&PatternSummary> {
        let indices = self.filtered_indices();
        indices
            .get(self.selected)
            .and_then(|&idx| self.patterns.get(idx))
    }

    pub fn update_from_summaries(&mut self, list: Vec<PatternSummary>) {
        self.patterns = list;
        let count = self.filtered_indices().len();
        if self.selected >= count && count > 0 {
            self.selected = count - 1;
        }
        self.update_current_detail_from_selection();
    }

    pub fn update_current_detail_from_selection(&mut self) {
        if let Some(sum) = self.selected_summary() {
            if let Ok((pat, src, path)) = shepherd_core::patterns::load_pattern(
                &sum.name,
                &std::path::PathBuf::from("."),
                None,
            ) {
                let config = shepherd_core::config::Config::default();
                let problems = validate(&pat, &config);
                self.current_detail = Some(PatternDetail {
                    pattern: pat,
                    source: src,
                    path: path.map(|p| p.display().to_string()),
                    problems,
                });
            } else if let Some(pat) = get_builtin_pattern(&sum.name) {
                let config = shepherd_core::config::Config::default();
                let problems = validate(&pat, &config);
                self.current_detail = Some(PatternDetail {
                    pattern: pat,
                    source: sum.source,
                    path: sum.path.clone(),
                    problems,
                });
            }
        }
    }

    pub fn update_detail(&mut self, detail: PatternDetail) {
        self.current_detail = Some(detail);
    }

    pub fn has_sub_action(&self) -> bool {
        self.sub_action.is_some()
    }

    pub fn cancel_sub_action(&mut self) -> bool {
        if self.sub_action.is_some() {
            self.sub_action = None;
            self.error = None;
            true
        } else {
            false
        }
    }

    pub fn select_prev(&mut self) {
        if self.selected > 0 {
            self.selected -= 1;
            self.detail_scroll = 0;
            if self.selected < self.list_scroll {
                self.list_scroll = self.selected;
            }
            self.update_current_detail_from_selection();
        }
    }

    pub fn select_next(&mut self) {
        let count = self.filtered_indices().len();
        if count > 0 && self.selected + 1 < count {
            self.selected += 1;
            self.detail_scroll = 0;
            self.update_current_detail_from_selection();
        }
    }

    pub fn start_new_from_template(&mut self) {
        self.sub_action = Some(PatternSubAction::NewFromTemplate(
            NewFromTemplateState::default(),
        ));
        self.error = None;
    }

    pub fn start_duplicate(&mut self) {
        if let Some(sum) = self.selected_summary() {
            self.sub_action = Some(PatternSubAction::Duplicate(DuplicatePatternState {
                source_name: sum.name.clone(),
                name: format!("{}-copia", sum.name),
                is_global: false,
                active_field: 0,
            }));
            self.error = None;
        }
    }

    pub fn start_edit(&mut self) {
        if let Some(detail) = &self.current_detail {
            self.sub_action = Some(PatternSubAction::Edit(PatternEditorState::from_pattern(
                &detail.pattern,
                detail.source,
            )));
            self.error = None;
        } else if let Some(sum) = self.selected_summary() {
            if let Some(pat) = get_builtin_pattern(&sum.name) {
                self.sub_action = Some(PatternSubAction::Edit(PatternEditorState::from_pattern(
                    &pat, sum.source,
                )));
                self.error = None;
            }
        }
    }

    pub fn start_delete(&mut self) {
        if let Some(sum) = self.selected_summary() {
            if sum.source == PatternSource::Builtin {
                self.error = Some(
                    "Las plantillas integradas del sistema son protegidas y no se pueden eliminar."
                        .into(),
                );
                return;
            }
            self.sub_action = Some(PatternSubAction::ConfirmDelete {
                name: sum.name.clone(),
                source: sum.source,
            });
            self.error = None;
        }
    }

    pub fn validate_selected(&mut self) -> Option<ShepherdPatternsAction> {
        if let Some(sum) = self.selected_summary() {
            Some(ShepherdPatternsAction::Validate {
                name: Some(sum.name.clone()),
                pattern: None,
            })
        } else {
            None
        }
    }

    pub fn start_run(&mut self) {
        if let Some(sum) = self.selected_summary() {
            self.sub_action = Some(PatternSubAction::Run(PatternRunFormState::new(
                sum.name.clone(),
            )));
            self.error = None;
        }
    }

    pub fn handle_key(&mut self, key: &KeyEvent) -> (bool, Option<ShepherdPatternsAction>) {
        if let Some(ref mut sub) = self.sub_action {
            match sub {
                PatternSubAction::ConfirmDelete { name, .. } => match key.code {
                    KeyCode::Esc | KeyCode::Char('c') => {
                        self.sub_action = None;
                        return (true, None);
                    }
                    KeyCode::Enter | KeyCode::Char('y') | KeyCode::Char('d') => {
                        let act = ShepherdPatternsAction::Delete { name: name.clone() };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                    _ => return (false, None),
                },
                PatternSubAction::NewFromTemplate(state) => match key.code {
                    KeyCode::Esc => {
                        self.sub_action = None;
                        return (true, None);
                    }
                    KeyCode::Tab => {
                        state.active_field = (state.active_field + 1) % 3;
                        return (true, None);
                    }
                    KeyCode::BackTab => {
                        state.active_field = if state.active_field == 0 {
                            2
                        } else {
                            state.active_field - 1
                        };
                        return (true, None);
                    }
                    KeyCode::Char(' ') => {
                        match state.active_field {
                            0 => {
                                state.kind_index = (state.kind_index + 1) % 4;
                                let kind_name = match state.kind_index {
                                    0 => "mi-orquestador",
                                    1 => "mi-jerarquico",
                                    2 => "mi-pizarra",
                                    _ => "mi-mercado",
                                };
                                state.name = kind_name.into();
                            }
                            2 => state.is_global = !state.is_global,
                            _ => state.name.push(' '),
                        }
                        return (true, None);
                    }
                    KeyCode::Backspace => {
                        if state.active_field == 1 {
                            state.name.pop();
                        }
                        return (true, None);
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.active_field == 1 {
                            state.name.push(c);
                        }
                        return (true, None);
                    }
                    KeyCode::Enter => {
                        let kind = state.selected_kind();
                        let name = state.name.trim().to_string();
                        let is_global = state.is_global;
                        if !name.is_empty() {
                            let editor = PatternEditorState::new_empty(kind, &name, is_global);
                            self.sub_action = Some(PatternSubAction::Edit(editor));
                        }
                        return (true, None);
                    }
                    _ => return (false, None),
                },
                PatternSubAction::Duplicate(state) => match key.code {
                    KeyCode::Esc => {
                        self.sub_action = None;
                        return (true, None);
                    }
                    KeyCode::Tab => {
                        state.active_field = (state.active_field + 1) % 2;
                        return (true, None);
                    }
                    KeyCode::BackTab => {
                        state.active_field = if state.active_field == 0 { 1 } else { 0 };
                        return (true, None);
                    }
                    KeyCode::Char(' ') if state.active_field == 1 => {
                        state.is_global = !state.is_global;
                        return (true, None);
                    }
                    KeyCode::Backspace => {
                        if state.active_field == 0 {
                            state.name.pop();
                        }
                        return (true, None);
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.active_field == 0 {
                            state.name.push(c);
                        }
                        return (true, None);
                    }
                    KeyCode::Enter => {
                        let src_name = state.source_name.clone();
                        let target_name = state.name.trim().to_string();
                        let is_global = state.is_global;
                        if !target_name.is_empty() {
                            let base_pat = get_builtin_pattern(&src_name).or_else(|| {
                                self.current_detail.as_ref().map(|d| d.pattern.clone())
                            });
                            if let Some(mut pat) = base_pat {
                                pat.name = target_name;
                                let mut editor = PatternEditorState::from_pattern(
                                    &pat,
                                    if is_global {
                                        PatternSource::Global
                                    } else {
                                        PatternSource::Project
                                    },
                                );
                                editor.is_global = is_global;
                                editor.is_new = true;
                                self.sub_action = Some(PatternSubAction::Edit(editor));
                            }
                        }
                        return (true, None);
                    }
                    _ => return (false, None),
                },
                PatternSubAction::Edit(editor) => match key.code {
                    KeyCode::Esc => {
                        self.sub_action = None;
                        return (true, None);
                    }
                    KeyCode::Tab => {
                        editor.cycle_forward();
                        return (true, None);
                    }
                    KeyCode::BackTab => {
                        editor.cycle_backward();
                        return (true, None);
                    }
                    KeyCode::Char('1') if key.modifiers.contains(KeyModifiers::ALT) => {
                        editor.active_section = EditorSection::Metadata;
                        editor.active_field = 0;
                        return (true, None);
                    }
                    KeyCode::Char('2') if key.modifiers.contains(KeyModifiers::ALT) => {
                        editor.active_section = EditorSection::Agents;
                        editor.active_field = 0;
                        return (true, None);
                    }
                    KeyCode::Char('3') if key.modifiers.contains(KeyModifiers::ALT) => {
                        editor.active_section = EditorSection::Topics;
                        editor.active_field = 0;
                        return (true, None);
                    }
                    KeyCode::Char('4') if key.modifiers.contains(KeyModifiers::ALT) => {
                        editor.active_section = EditorSection::Topology;
                        editor.active_field = 0;
                        return (true, None);
                    }
                    KeyCode::Char(' ')
                        if (editor.active_section == EditorSection::Metadata
                            && (editor.active_field == 1 || editor.active_field == 2))
                            || (editor.active_section == EditorSection::Agents
                                && editor.active_field == 10) =>
                    {
                        editor.toggle_active_choice();
                        return (true, None);
                    }
                    KeyCode::Char('[') if key.modifiers.contains(KeyModifiers::ALT) => {
                        if editor.active_section == EditorSection::Agents
                            && editor.selected_agent > 0
                        {
                            editor.selected_agent -= 1;
                            editor.active_field = 0;
                            return (true, None);
                        }
                        if editor.active_section == EditorSection::Topics
                            && editor.selected_topic > 0
                        {
                            editor.selected_topic -= 1;
                            editor.active_field = 0;
                            return (true, None);
                        }
                    }
                    KeyCode::Char(']') if key.modifiers.contains(KeyModifiers::ALT) => {
                        if editor.active_section == EditorSection::Agents
                            && editor.selected_agent + 1 < editor.agents.len()
                        {
                            editor.selected_agent += 1;
                            editor.active_field = 0;
                            return (true, None);
                        }
                        if editor.active_section == EditorSection::Topics
                            && editor.selected_topic + 1 < editor.topics.len()
                        {
                            editor.selected_topic += 1;
                            editor.active_field = 0;
                            return (true, None);
                        }
                    }
                    KeyCode::Char('+') | KeyCode::Char('A')
                        if editor.active_section == EditorSection::Agents
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.add_agent();
                        return (true, None);
                    }
                    KeyCode::Char('-') | KeyCode::Char('X')
                        if editor.active_section == EditorSection::Agents
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.remove_agent();
                        return (true, None);
                    }
                    KeyCode::Char('K')
                        if editor.active_section == EditorSection::Agents
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.move_agent_up();
                        return (true, None);
                    }
                    KeyCode::Char('J')
                        if editor.active_section == EditorSection::Agents
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.move_agent_down();
                        return (true, None);
                    }
                    KeyCode::Char('+') | KeyCode::Char('T')
                        if editor.active_section == EditorSection::Topics
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.add_topic();
                        return (true, None);
                    }
                    KeyCode::Char('-') | KeyCode::Char('D')
                        if editor.active_section == EditorSection::Topics
                            && key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.remove_topic();
                        return (true, None);
                    }
                    KeyCode::Backspace => {
                        editor.delete_char();
                        return (true, None);
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        editor.insert_char(c);
                        return (true, None);
                    }
                    KeyCode::Enter | KeyCode::Char('s')
                        if key.modifiers.contains(KeyModifiers::CONTROL)
                            || (key.code == KeyCode::Enter
                                && editor.active_field
                                    == editor.field_count_for_section().saturating_sub(1)) =>
                    {
                        if editor.is_valid() {
                            let pat = editor.to_pattern();
                            let is_global = editor.is_global;
                            let act = ShepherdPatternsAction::Save {
                                pattern: pat,
                                global: is_global,
                            };
                            self.sub_action = None;
                            return (true, Some(act));
                        } else {
                            self.error = Some(
                                "No se puede guardar: corrija los errores de validación.".into(),
                            );
                            return (true, None);
                        }
                    }
                    _ => return (false, None),
                },
                PatternSubAction::Run(state) => match key.code {
                    KeyCode::Esc => {
                        self.sub_action = None;
                        return (true, None);
                    }
                    KeyCode::Tab => {
                        state.active_field = (state.active_field + 1) % 2;
                        return (true, None);
                    }
                    KeyCode::BackTab => {
                        state.active_field = if state.active_field == 0 { 1 } else { 0 };
                        return (true, None);
                    }
                    KeyCode::Backspace => {
                        if state.active_field == 0 {
                            state.task.pop();
                        } else {
                            state.worktree.pop();
                        }
                        return (true, None);
                    }
                    KeyCode::Char(c)
                        if !key.modifiers.contains(KeyModifiers::CONTROL)
                            && !key.modifiers.contains(KeyModifiers::ALT) =>
                    {
                        if state.active_field == 0 {
                            state.task.push(c);
                        } else {
                            state.worktree.push(c);
                        }
                        return (true, None);
                    }
                    KeyCode::Enter => {
                        let pattern = state.pattern_name.clone();
                        let task = if state.task.trim().is_empty() {
                            None
                        } else {
                            Some(state.task.trim().to_string())
                        };
                        let worktree = if state.worktree.trim().is_empty() {
                            None
                        } else {
                            Some(state.worktree.trim().to_string())
                        };
                        let act = ShepherdPatternsAction::RunPattern {
                            pattern,
                            task,
                            worktree,
                        };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                    _ => return (false, None),
                },
            }
        }

        match key.code {
            KeyCode::Up | KeyCode::Char('k') => {
                self.select_prev();
                let act = self
                    .selected_summary()
                    .map(|s| ShepherdPatternsAction::Get {
                        name: s.name.clone(),
                    });
                (true, act)
            }
            KeyCode::Down | KeyCode::Char('j') => {
                self.select_next();
                let act = self
                    .selected_summary()
                    .map(|s| ShepherdPatternsAction::Get {
                        name: s.name.clone(),
                    });
                (true, act)
            }
            KeyCode::Char('n') => {
                self.start_new_from_template();
                (true, None)
            }
            KeyCode::Char('c') => {
                self.start_duplicate();
                (true, None)
            }
            KeyCode::Char('e') => {
                self.start_edit();
                (true, None)
            }
            KeyCode::Char('r') => {
                self.start_run();
                (true, None)
            }
            KeyCode::Char('d') => {
                self.start_delete();
                (true, None)
            }
            KeyCode::Char('v') => {
                let act = self.validate_selected();
                (true, act)
            }
            KeyCode::Char('f') => {
                self.filter_index = (self.filter_index + 1) % 4;
                self.selected = 0;
                self.update_current_detail_from_selection();
                (true, None)
            }
            KeyCode::PageUp => {
                self.detail_scroll = self.detail_scroll.saturating_sub(5);
                (true, None)
            }
            KeyCode::PageDown => {
                self.detail_scroll = self.detail_scroll.saturating_add(5);
                (true, None)
            }
            _ => (false, None),
        }
    }

    pub fn handle_mouse(
        &mut self,
        col: u16,
        row: u16,
        hits: &ShepherdPatternsHits,
    ) -> (bool, Option<ShepherdPatternsAction>) {
        if let Some(ref sh) = hits.stripe_hits {
            if let Some(c_rect) = sh.confirm_button {
                if c_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::ConfirmDelete { ref name, .. }) = self.sub_action
                    {
                        let act = ShepherdPatternsAction::Delete { name: name.clone() };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                }
            }
            if let Some(cancel_rect) = sh.cancel_button {
                if cancel_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        if let Some(ref eh) = hits.editor_hits {
            for (rect, idx) in &eh.tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.active_section = EditorSection::from_index(*idx);
                        ed.active_field = 0;
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.field_rects {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.active_field = *idx;
                        // The start checkbox flips on click (like the Kind/Target choices).
                        if ed.active_section == EditorSection::Agents && *idx == 10 {
                            ed.toggle_active_choice();
                        }
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.agent_tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.selected_agent = *idx;
                        return (true, None);
                    }
                }
            }
            for (rect, idx) in &eh.topic_tabs {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.selected_topic = *idx;
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.add_agent_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.add_agent();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.remove_agent_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.remove_agent();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.move_agent_up_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.move_agent_up();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.move_agent_down_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.move_agent_down();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.add_topic_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.add_topic();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.remove_topic_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref mut ed)) = self.sub_action {
                        ed.remove_topic();
                        return (true, None);
                    }
                }
            }
            if let Some(r) = eh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Edit(ref ed)) = self.sub_action {
                        if ed.is_valid() {
                            let act = ShepherdPatternsAction::Save {
                                pattern: ed.to_pattern(),
                                global: ed.is_global,
                            };
                            self.sub_action = None;
                            return (true, Some(act));
                        }
                    }
                }
            }
            if let Some(r) = eh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        if let Some(ref nfh) = hits.new_from_template_hits {
            if let Some(r) = nfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::NewFromTemplate(ref st)) = self.sub_action {
                        let kind = st.selected_kind();
                        let name = st.name.trim().to_string();
                        let is_global = st.is_global;
                        if !name.is_empty() {
                            let editor = PatternEditorState::new_empty(kind, &name, is_global);
                            self.sub_action = Some(PatternSubAction::Edit(editor));
                            return (true, None);
                        }
                    }
                }
            }
            if let Some(r) = nfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        if let Some(ref dfh) = hits.duplicate_hits {
            if let Some(r) = dfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Duplicate(ref st)) = self.sub_action {
                        let src_name = st.source_name.clone();
                        let target_name = st.name.trim().to_string();
                        let is_global = st.is_global;
                        if !target_name.is_empty() {
                            let base_pat = get_builtin_pattern(&src_name).or_else(|| {
                                self.current_detail.as_ref().map(|d| d.pattern.clone())
                            });
                            if let Some(mut pat) = base_pat {
                                pat.name = target_name;
                                let mut editor = PatternEditorState::from_pattern(
                                    &pat,
                                    if is_global {
                                        PatternSource::Global
                                    } else {
                                        PatternSource::Project
                                    },
                                );
                                editor.is_global = is_global;
                                editor.is_new = true;
                                self.sub_action = Some(PatternSubAction::Edit(editor));
                                return (true, None);
                            }
                        }
                    }
                }
            }
            if let Some(r) = dfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
        }

        if let Some(ref rfh) = hits.run_form_hits {
            if let Some(r) = rfh.submit_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Run(ref st)) = self.sub_action {
                        let pattern = st.pattern_name.clone();
                        let task = if st.task.trim().is_empty() {
                            None
                        } else {
                            Some(st.task.trim().to_string())
                        };
                        let worktree = if st.worktree.trim().is_empty() {
                            None
                        } else {
                            Some(st.worktree.trim().to_string())
                        };
                        let act = ShepherdPatternsAction::RunPattern {
                            pattern,
                            task,
                            worktree,
                        };
                        self.sub_action = None;
                        return (true, Some(act));
                    }
                }
            }
            if let Some(r) = rfh.cancel_button {
                if r.contains(ratatui::layout::Position { x: col, y: row }) {
                    self.sub_action = None;
                    return (true, None);
                }
            }
            for (f_rect, idx) in &rfh.field_rects {
                if f_rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if let Some(PatternSubAction::Run(ref mut st)) = self.sub_action {
                        st.active_field = *idx;
                        return (true, None);
                    }
                }
            }
        }

        if let Some(ref eh) = hits.empty_hits {
            for (rect, idx) in &eh.actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    if *idx == 0 {
                        self.start_new_from_template();
                        return (true, None);
                    }
                }
            }
        }

        for (rect, row_idx) in &hits.list_hits.rows {
            if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                self.selected = *row_idx;
                self.detail_scroll = 0;
                self.update_current_detail_from_selection();
                let act = self
                    .selected_summary()
                    .map(|s| ShepherdPatternsAction::Get {
                        name: s.name.clone(),
                    });
                return (true, act);
            }
        }
        for (rect, tab_idx) in &hits.list_hits.filter_tabs {
            if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                self.filter_index = *tab_idx;
                self.selected = 0;
                self.update_current_detail_from_selection();
                return (true, None);
            }
        }
        if let Some(btn) = hits.list_hits.action_button {
            if btn.contains(ratatui::layout::Position { x: col, y: row }) {
                self.start_new_from_template();
                return (true, None);
            }
        }

        if let Some(ref dh) = hits.detail_hits {
            for (rect, idx) in &dh.header_actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    match idx {
                        0 => {
                            self.start_edit();
                            return (true, None);
                        }
                        1 => {
                            self.start_duplicate();
                            return (true, None);
                        }
                        2 => {
                            let act = self.validate_selected();
                            return (true, act);
                        }
                        3 => {
                            self.start_delete();
                            return (true, None);
                        }
                        4 => {
                            self.start_run();
                            return (true, None);
                        }
                        _ => {}
                    }
                }
            }
            for (rect, idx) in &dh.bottom_actions {
                if rect.contains(ratatui::layout::Position { x: col, y: row }) {
                    match idx {
                        0 => {
                            self.start_edit();
                            return (true, None);
                        }
                        1 => {
                            self.start_duplicate();
                            return (true, None);
                        }
                        2 => {
                            let act = self.validate_selected();
                            return (true, act);
                        }
                        3 => {
                            self.start_delete();
                            return (true, None);
                        }
                        4 => {
                            self.start_run();
                            return (true, None);
                        }
                        _ => {}
                    }
                }
            }
        }

        (false, None)
    }
}
