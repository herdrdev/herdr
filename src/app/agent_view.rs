use std::borrow::Cow;

use crate::agent_view_eval::{AgentViewContext as EvaluationContext, AgentViewEntry};
use crate::api::schema::{
    AgentViewBuiltinField, AgentViewContext, AgentViewField, AgentViewFilter, AgentViewSetParams,
    AgentViewSortField, AgentViewValue,
};
use crate::ui::AgentPanelEntry;

use super::AppState;

const MAX_FILTER_DEPTH: usize = 8;
const MAX_FILTER_NODES: usize = 64;
const MAX_FILTER_VALUES: usize = 32;
const MAX_SORT_FIELDS: usize = 8;
const MAX_SOURCE_CHARS: usize = 120;
const MAX_LABEL_CHARS: usize = 32;

pub(crate) fn validate_agent_view(spec: &mut AgentViewSetParams) -> Result<(), String> {
    spec.source = normalize_source(&spec.source)?;
    spec.label = spec
        .label
        .take()
        .map(|label| normalize_label(&label))
        .transpose()?;

    let mut nodes = 0;
    if let Some(filter) = &spec.filter {
        validate_filter(filter, 1, &mut nodes)?;
    }
    if spec.sort.len() > MAX_SORT_FIELDS {
        return Err(format!(
            "agent view sort may contain at most {MAX_SORT_FIELDS} fields"
        ));
    }
    for sort in &spec.sort {
        validate_sort_field(&sort.field)?;
    }
    Ok(())
}

pub(crate) fn validate_agent_view_source(source: &str) -> Result<String, String> {
    normalize_source(source)
}

pub(crate) fn apply_agent_view(app: &AppState, entries: &mut Vec<AgentPanelEntry>) {
    if let Some(spec) = app.agent_view_override.as_ref() {
        let context = evaluation_context(app);
        if let Some(filter) = &spec.filter {
            entries.retain(|entry| {
                crate::agent_view_eval::matches_filter(
                    &context,
                    &AppAgentViewEntry { app, entry },
                    filter,
                )
            });
        }
        if !spec.sort.is_empty() {
            entries.sort_by(|left, right| {
                crate::agent_view_eval::compare_entries(
                    &AppAgentViewEntry { app, entry: left },
                    &AppAgentViewEntry { app, entry: right },
                    &spec.sort,
                )
            });
            return;
        }
    }

    if matches!(
        app.agent_panel_sort,
        crate::app::state::AgentPanelSort::Priority
    ) {
        entries.sort_by_key(|entry| {
            (
                std::cmp::Reverse(super::api_helpers::tab_attention_priority(
                    entry.state,
                    entry.seen,
                )),
                std::cmp::Reverse(entry.last_agent_state_change_seq),
            )
        });
    }
}

pub(crate) fn presented_workspace_idx(app: &AppState) -> Option<usize> {
    app.active
}

fn evaluation_context(app: &AppState) -> EvaluationContext {
    let workspace = presented_workspace_idx(app).and_then(|index| app.workspaces.get(index));
    EvaluationContext {
        scope: 0,
        workspace_id: workspace.map(|workspace| workspace.id.clone()),
        tab_id: workspace.and_then(|workspace| {
            let number = workspace.public_tab_number(workspace.active_tab)?;
            Some(crate::workspace::public_tab_id_for_number(
                &workspace.id,
                number,
            ))
        }),
    }
}

struct AppAgentViewEntry<'a> {
    app: &'a AppState,
    entry: &'a AgentPanelEntry,
}

impl AgentViewEntry for AppAgentViewEntry<'_> {
    fn scope(&self) -> usize {
        0
    }

    fn status(&self) -> &'static str {
        status_name(self.entry.state, self.entry.seen)
    }

    fn workspace_id(&self) -> Option<Cow<'_, str>> {
        self.app
            .workspaces
            .get(self.entry.ws_idx)
            .map(|workspace| Cow::Borrowed(workspace.id.as_str()))
    }

    fn tab_id(&self) -> Option<Cow<'_, str>> {
        public_tab_id(self.app, self.entry).map(Cow::Owned)
    }

    fn pane_id(&self) -> Option<Cow<'_, str>> {
        public_pane_id(self.app, self.entry).map(Cow::Owned)
    }

    fn agent(&self) -> Option<&str> {
        self.entry.agent_kind_label.as_deref()
    }

    fn seen(&self) -> bool {
        self.entry.seen
    }

    fn state_change_seq(&self) -> Option<u64> {
        self.entry.last_agent_state_change_seq
    }

    fn token(&self, token: &str) -> Option<&str> {
        self.entry.tokens.get(token).map(String::as_str)
    }

    fn workspace_order(&self) -> Option<u64> {
        Some(self.entry.ws_idx as u64)
    }

    fn tab_order(&self) -> Option<u64> {
        self.app
            .workspaces
            .get(self.entry.ws_idx)
            .and_then(|workspace| workspace.public_tab_number(self.entry.tab_idx))
            .map(|number| number as u64)
    }

    fn pane_order(&self) -> Option<u64> {
        self.app
            .workspaces
            .get(self.entry.ws_idx)
            .and_then(|workspace| workspace.public_pane_number(self.entry.pane_id))
            .map(|number| number as u64)
    }

    fn attention(&self) -> u64 {
        u64::from(super::api_helpers::tab_attention_priority(
            self.entry.state,
            self.entry.seen,
        ))
    }
}

fn normalize_source(source: &str) -> Result<String, String> {
    let source = source.trim();
    if source.is_empty()
        || source.chars().count() > MAX_SOURCE_CHARS
        || !source
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, ':' | '.' | '_' | '-'))
    {
        return Err(format!(
            "agent view source must be non-empty, at most {MAX_SOURCE_CHARS} characters, and contain only ASCII letters, digits, colon, dot, underscore, or hyphen"
        ));
    }
    Ok(source.to_string())
}

fn normalize_label(label: &str) -> Result<String, String> {
    let label = label
        .trim()
        .chars()
        .filter(|ch| !ch.is_control())
        .collect::<String>();
    if label.is_empty() || label.chars().count() > MAX_LABEL_CHARS {
        return Err(format!(
            "agent view label must be non-empty and at most {MAX_LABEL_CHARS} characters"
        ));
    }
    Ok(label)
}

fn validate_filter(
    filter: &AgentViewFilter,
    depth: usize,
    nodes: &mut usize,
) -> Result<(), String> {
    if depth > MAX_FILTER_DEPTH {
        return Err(format!(
            "agent view filter may be nested at most {MAX_FILTER_DEPTH} levels"
        ));
    }
    *nodes += 1;
    if *nodes > MAX_FILTER_NODES {
        return Err(format!(
            "agent view filter may contain at most {MAX_FILTER_NODES} nodes"
        ));
    }

    match filter {
        AgentViewFilter::All { filters } | AgentViewFilter::Any { filters } => {
            if filters.is_empty() {
                return Err("agent view all/any filters must not be empty".to_string());
            }
            for filter in filters {
                validate_filter(filter, depth + 1, nodes)?;
            }
        }
        AgentViewFilter::Not { filter } => validate_filter(filter, depth + 1, nodes)?,
        AgentViewFilter::Eq { field, value } => validate_field_value(field, value)?,
        AgentViewFilter::In { field, values } => {
            if values.is_empty() || values.len() > MAX_FILTER_VALUES {
                return Err(format!(
                    "agent view in filters require 1 to {MAX_FILTER_VALUES} values"
                ));
            }
            for value in values {
                validate_field_value(field, value)?;
            }
        }
        AgentViewFilter::Exists { field } => validate_field(field)?,
    }
    Ok(())
}

fn validate_field(field: &AgentViewField) -> Result<(), String> {
    if let AgentViewField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_field_value(field: &AgentViewField, value: &AgentViewValue) -> Result<(), String> {
    validate_field(field)?;
    match (field, value) {
        (
            AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentWorkspaceId,
            },
        )
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::TabId),
            AgentViewValue::Context {
                context: AgentViewContext::CurrentTabId,
            },
        ) => Ok(()),
        (_, AgentViewValue::Context { .. }) => {
            Err("agent view context type does not match the selected field".to_string())
        }
        (AgentViewField::Builtin(AgentViewBuiltinField::Seen), AgentViewValue::Bool(_))
        | (
            AgentViewField::Builtin(AgentViewBuiltinField::StateChangeSeq),
            AgentViewValue::Number(_),
        ) => Ok(()),
        (
            AgentViewField::Builtin(
                AgentViewBuiltinField::Status
                | AgentViewBuiltinField::WorkspaceId
                | AgentViewBuiltinField::TabId
                | AgentViewBuiltinField::PaneId
                | AgentViewBuiltinField::Agent,
            )
            | AgentViewField::Token { .. },
            AgentViewValue::String(value),
        ) => {
            if matches!(
                field,
                AgentViewField::Builtin(AgentViewBuiltinField::Status)
            ) && !matches!(
                value.as_str(),
                "idle" | "working" | "blocked" | "done" | "unknown"
            ) {
                return Err(format!("unknown agent status `{value}`"));
            }
            Ok(())
        }
        _ => Err("agent view value type does not match the selected field".to_string()),
    }
}

fn validate_sort_field(field: &AgentViewSortField) -> Result<(), String> {
    if let AgentViewSortField::Token { token } = field {
        validate_token(token)?;
    }
    Ok(())
}

fn validate_token(token: &str) -> Result<(), String> {
    if token.is_empty()
        || token.len() > 32
        || !token
            .chars()
            .all(|ch| ch.is_ascii_alphanumeric() || matches!(ch, '_' | '-'))
    {
        return Err(format!("invalid agent view token `{token}`"));
    }
    Ok(())
}

fn status_name(state: crate::detect::AgentState, seen: bool) -> &'static str {
    match (state, seen) {
        (crate::detect::AgentState::Idle, false) => "done",
        (crate::detect::AgentState::Idle, true) => "idle",
        (crate::detect::AgentState::Working, _) => "working",
        (crate::detect::AgentState::Blocked, _) => "blocked",
        (crate::detect::AgentState::Unknown, _) => "unknown",
    }
}

fn public_tab_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_tab_number(entry.tab_idx)?;
    Some(crate::workspace::public_tab_id_for_number(
        &workspace.id,
        number,
    ))
}

fn public_pane_id(app: &AppState, entry: &AgentPanelEntry) -> Option<String> {
    let workspace = app.workspaces.get(entry.ws_idx)?;
    let number = workspace.public_pane_number(entry.pane_id)?;
    Some(crate::workspace::public_pane_id_for_number(
        &workspace.id,
        number,
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::schema::{
        AgentViewBuiltinSortField, AgentViewSort, AgentViewSortField, AgentViewSortOrder,
    };
    use crate::detect::{Agent, AgentState};
    use crate::workspace::Workspace;

    fn state_with_agents() -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("one"), Workspace::test_new("two")];
        state.ensure_test_terminals();
        state.active = Some(0);
        state.selected = 0;
        for (ws_idx, agent_state) in [(0, AgentState::Idle), (1, AgentState::Working)] {
            let pane_id = state.workspaces[ws_idx].tabs[0].root_pane;
            let terminal_id = state.workspaces[ws_idx].tabs[0].panes[&pane_id]
                .attached_terminal_id
                .clone();
            let terminal = state.terminals.get_mut(&terminal_id).unwrap();
            terminal.detected_agent = Some(Agent::Claude);
            terminal.state = agent_state;
        }
        state
    }

    fn projected_entries(state: &AppState) -> Vec<crate::ui::AgentPanelEntry> {
        crate::ui::agent_panel_entries_from(state, &crate::terminal::TerminalRuntimeRegistry::new())
    }

    fn current_workspace_view() -> AgentViewSetParams {
        AgentViewSetParams {
            source: "example.views".to_string(),
            label: Some("current".to_string()),
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                value: AgentViewValue::Context {
                    context: AgentViewContext::CurrentWorkspaceId,
                },
            }),
            sort: Vec::new(),
        }
    }

    #[test]
    fn current_workspace_filter_tracks_presented_workspace() {
        let mut state = state_with_agents();
        state.agent_view_override = Some(current_workspace_view());

        assert_eq!(projected_entries(&state)[0].ws_idx, 0);

        state.active = Some(1);
        let entries = projected_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ws_idx, 1);

        state.active = Some(0);
        let entries = projected_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].ws_idx, 0);
    }

    #[test]
    fn boolean_filter_and_custom_sort_define_canonical_entries() {
        let mut state = state_with_agents();
        let first_pane = state.workspaces[0].tabs[0].root_pane;
        let first_terminal = state.workspaces[0].tabs[0].panes[&first_pane]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&first_terminal).unwrap().state = AgentState::Working;
        state.agent_view_override = Some(AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(AgentViewFilter::All {
                filters: vec![
                    AgentViewFilter::Eq {
                        field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                        value: AgentViewValue::String("working".to_string()),
                    },
                    AgentViewFilter::Not {
                        filter: Box::new(AgentViewFilter::Eq {
                            field: AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId),
                            value: AgentViewValue::String("missing".to_string()),
                        }),
                    },
                ],
            }),
            sort: vec![AgentViewSort {
                field: AgentViewSortField::Builtin(AgentViewBuiltinSortField::WorkspaceOrder),
                order: AgentViewSortOrder::Desc,
            }],
        });

        let entries = projected_entries(&state);
        assert_eq!(entries.len(), 2);
        assert_eq!(entries[0].ws_idx, 1);
        assert_eq!(entries[1].ws_idx, 0);
    }

    #[test]
    fn agent_filter_matches_custom_lifecycle_agent_label() {
        let mut state = state_with_agents();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_hook_authority(
                "test".to_string(),
                "custom-agent".to_string(),
                AgentState::Working,
                None,
                None,
            );
        state.agent_view_override = Some(AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Agent),
                value: AgentViewValue::String("custom-agent".to_string()),
            }),
            sort: Vec::new(),
        });

        let entries = projected_entries(&state);
        assert_eq!(entries.len(), 1);
        assert_eq!(entries[0].agent_kind_label.as_deref(), Some("custom-agent"));
    }

    #[test]
    fn validation_rejects_mismatched_context_type() {
        let mut spec = AgentViewSetParams {
            source: "example.views".to_string(),
            label: None,
            filter: Some(AgentViewFilter::Eq {
                field: AgentViewField::Builtin(AgentViewBuiltinField::Status),
                value: AgentViewValue::Context {
                    context: AgentViewContext::CurrentWorkspaceId,
                },
            }),
            sort: Vec::new(),
        };

        assert!(validate_agent_view(&mut spec)
            .unwrap_err()
            .contains("context type"));
    }
}
