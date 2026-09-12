use std::borrow::Cow;
use std::cmp::Ordering;

use crate::api::schema::{
    AgentViewBuiltinField, AgentViewBuiltinSortField, AgentViewField, AgentViewFilter,
    AgentViewSort, AgentViewSortField, AgentViewSortOrder, AgentViewValue,
};

pub(crate) struct AgentViewContext {
    pub(crate) scope: usize,
    pub(crate) workspace_id: Option<String>,
    pub(crate) tab_id: Option<String>,
}

pub(crate) trait AgentViewEntry {
    fn scope(&self) -> usize;
    fn status(&self) -> &'static str;
    fn workspace_id(&self) -> Option<Cow<'_, str>>;
    fn tab_id(&self) -> Option<Cow<'_, str>>;
    fn pane_id(&self) -> Option<Cow<'_, str>>;
    fn agent(&self) -> Option<&str>;
    fn seen(&self) -> bool;
    fn state_change_seq(&self) -> Option<u64>;
    fn token(&self, token: &str) -> Option<&str>;
    fn workspace_order(&self) -> Option<u64>;
    fn tab_order(&self) -> Option<u64>;
    fn pane_order(&self) -> Option<u64>;
    fn attention(&self) -> u64;
}

#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
enum EvalValue<'a> {
    String(Cow<'a, str>),
    ScopedId { scope: usize, value: Cow<'a, str> },
    Bool(bool),
    Number(u64),
}

pub(crate) fn matches_filter<E: AgentViewEntry + ?Sized>(
    context: &AgentViewContext,
    entry: &E,
    filter: &AgentViewFilter,
) -> bool {
    match filter {
        AgentViewFilter::All { filters } => filters
            .iter()
            .all(|filter| matches_filter(context, entry, filter)),
        AgentViewFilter::Any { filters } => filters
            .iter()
            .any(|filter| matches_filter(context, entry, filter)),
        AgentViewFilter::Not { filter } => !matches_filter(context, entry, filter),
        AgentViewFilter::Eq { field, value } => {
            field_value(entry, field) == operand_value(context, field, value)
        }
        AgentViewFilter::In { field, values } => {
            let actual = field_value(entry, field);
            values
                .iter()
                .any(|value| actual == operand_value(context, field, value))
        }
        AgentViewFilter::Exists { field } => field_value(entry, field).is_some(),
    }
}

pub(crate) fn compare_entries<E: AgentViewEntry + ?Sized>(
    left: &E,
    right: &E,
    sorts: &[AgentViewSort],
) -> Ordering {
    for sort in sorts {
        let ordering = compare_optional_values(
            sort_value(left, &sort.field),
            sort_value(right, &sort.field),
            sort.order,
        );
        if ordering != Ordering::Equal {
            return ordering;
        }
    }
    Ordering::Equal
}

fn field_value<'a, E: AgentViewEntry + ?Sized>(
    entry: &'a E,
    field: &AgentViewField,
) -> Option<EvalValue<'a>> {
    match field {
        AgentViewField::Builtin(AgentViewBuiltinField::Status) => {
            Some(EvalValue::String(Cow::Borrowed(entry.status())))
        }
        AgentViewField::Builtin(AgentViewBuiltinField::WorkspaceId) => {
            entry.workspace_id().map(|value| EvalValue::ScopedId {
                scope: entry.scope(),
                value,
            })
        }
        AgentViewField::Builtin(AgentViewBuiltinField::TabId) => {
            entry.tab_id().map(|value| EvalValue::ScopedId {
                scope: entry.scope(),
                value,
            })
        }
        AgentViewField::Builtin(AgentViewBuiltinField::PaneId) => {
            entry.pane_id().map(|value| EvalValue::ScopedId {
                scope: entry.scope(),
                value,
            })
        }
        AgentViewField::Builtin(AgentViewBuiltinField::Agent) => entry
            .agent()
            .map(|value| EvalValue::String(Cow::Borrowed(value))),
        AgentViewField::Builtin(AgentViewBuiltinField::Seen) => Some(EvalValue::Bool(entry.seen())),
        AgentViewField::Builtin(AgentViewBuiltinField::StateChangeSeq) => {
            entry.state_change_seq().map(EvalValue::Number)
        }
        AgentViewField::Token { token } => entry
            .token(token)
            .map(|value| EvalValue::String(Cow::Borrowed(value))),
    }
}

fn operand_value<'a>(
    context: &'a AgentViewContext,
    field: &AgentViewField,
    value: &'a AgentViewValue,
) -> Option<EvalValue<'a>> {
    match value {
        AgentViewValue::String(value) if is_id_field(field) => Some(EvalValue::ScopedId {
            scope: context.scope,
            value: Cow::Borrowed(value),
        }),
        AgentViewValue::String(value) => Some(EvalValue::String(Cow::Borrowed(value))),
        AgentViewValue::Bool(value) => Some(EvalValue::Bool(*value)),
        AgentViewValue::Number(value) => Some(EvalValue::Number(*value)),
        AgentViewValue::Context {
            context: view_context,
        } => match view_context {
            crate::api::schema::AgentViewContext::CurrentWorkspaceId => context
                .workspace_id
                .as_deref()
                .map(|value| EvalValue::ScopedId {
                    scope: context.scope,
                    value: Cow::Borrowed(value),
                }),
            crate::api::schema::AgentViewContext::CurrentTabId => {
                context.tab_id.as_deref().map(|value| EvalValue::ScopedId {
                    scope: context.scope,
                    value: Cow::Borrowed(value),
                })
            }
        },
    }
}

fn is_id_field(field: &AgentViewField) -> bool {
    matches!(
        field,
        AgentViewField::Builtin(
            AgentViewBuiltinField::WorkspaceId
                | AgentViewBuiltinField::TabId
                | AgentViewBuiltinField::PaneId
        )
    )
}

fn sort_value<'a, E: AgentViewEntry + ?Sized>(
    entry: &'a E,
    field: &AgentViewSortField,
) -> Option<EvalValue<'a>> {
    match field {
        AgentViewSortField::Token { token } => entry
            .token(token)
            .map(|value| EvalValue::String(Cow::Borrowed(value))),
        AgentViewSortField::Builtin(field) => match field {
            AgentViewBuiltinSortField::WorkspaceOrder => {
                entry.workspace_order().map(EvalValue::Number)
            }
            AgentViewBuiltinSortField::TabOrder => entry.tab_order().map(EvalValue::Number),
            AgentViewBuiltinSortField::PaneOrder => entry.pane_order().map(EvalValue::Number),
            AgentViewBuiltinSortField::Attention => Some(EvalValue::Number(entry.attention())),
            AgentViewBuiltinSortField::Status => {
                Some(EvalValue::String(Cow::Borrowed(entry.status())))
            }
            AgentViewBuiltinSortField::Agent => entry
                .agent()
                .map(|value| EvalValue::String(Cow::Borrowed(value))),
            AgentViewBuiltinSortField::Seen => Some(EvalValue::Bool(entry.seen())),
            AgentViewBuiltinSortField::StateChangeSeq => {
                entry.state_change_seq().map(EvalValue::Number)
            }
        },
    }
}

fn compare_optional_values(
    left: Option<EvalValue<'_>>,
    right: Option<EvalValue<'_>>,
    order: AgentViewSortOrder,
) -> Ordering {
    match (left, right) {
        (Some(left), Some(right)) => {
            let ordering = left.cmp(&right);
            if matches!(order, AgentViewSortOrder::Desc) {
                ordering.reverse()
            } else {
                ordering
            }
        }
        (Some(_), None) => Ordering::Less,
        (None, Some(_)) => Ordering::Greater,
        (None, None) => Ordering::Equal,
    }
}
