//! Derived agent notification policy: sounds, toasts, and display context.
//!
//! These helpers do not mutate AppState. Completion detection is shared with
//! pane `seen` marking because both answer "did this pane just finish work".

use crate::detect::AgentState;
use crate::layout::PaneId;
use crate::terminal::EffectiveStateChange;

use super::state::ToastKind;

fn is_background_completion_transition(prev_state: AgentState, new_state: AgentState) -> bool {
    matches!(new_state, AgentState::Idle)
        && matches!(prev_state, AgentState::Working | AgentState::Blocked)
}

pub(super) fn is_completion_transition(change: &EffectiveStateChange) -> bool {
    is_completion_transition_parts(
        change.previous_state,
        change.state,
        change.previous_agent_label.as_deref(),
        change.agent_label.as_deref(),
    )
}

fn is_completion_transition_parts(
    previous_state: AgentState,
    state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> bool {
    is_background_completion_transition(previous_state, state)
        || (previous_state == AgentState::Unknown
            && state == AgentState::Idle
            && previous_agent_label.is_some()
            && previous_agent_label == agent_label)
}

pub fn active_tab_suppresses_notifications(
    is_active_tab: bool,
    outer_terminal_focus: Option<bool>,
) -> bool {
    is_active_tab && outer_terminal_focus != Some(false)
}

#[cfg(test)]
fn notification_sound_for_state_change(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_background_completion_transition(prev_state, new_state)
                && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_sound_for_state_change_with_agent_labels(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> Option<crate::sound::Sound> {
    if new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_completion_transition_parts(
                prev_state,
                new_state,
                previous_agent_label,
                agent_label,
            ) && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub(super) fn notification_sound_for_effective_state_change(
    suppress_active_tab_notifications: bool,
    change: &EffectiveStateChange,
) -> Option<crate::sound::Sound> {
    if change.state == change.previous_state {
        return None;
    }

    match change.state {
        AgentState::Blocked => Some(crate::sound::Sound::Request),
        AgentState::Idle
            if is_completion_transition(change) && !suppress_active_tab_notifications =>
        {
            Some(crate::sound::Sound::Done)
        }
        _ => None,
    }
}

pub fn notification_toast_for_state_change_with_agent_labels(
    suppress_active_tab_notifications: bool,
    prev_state: AgentState,
    new_state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || new_state == prev_state {
        return None;
    }

    match new_state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle
            if is_completion_transition_parts(
                prev_state,
                new_state,
                previous_agent_label,
                agent_label,
            ) =>
        {
            Some(ToastKind::Finished)
        }
        _ => None,
    }
}

pub(super) fn notification_toast_for_effective_state_change(
    suppress_active_tab_notifications: bool,
    change: &EffectiveStateChange,
) -> Option<ToastKind> {
    if suppress_active_tab_notifications || change.state == change.previous_state {
        return None;
    }

    match change.state {
        AgentState::Blocked => Some(ToastKind::NeedsAttention),
        AgentState::Idle if is_completion_transition(change) => Some(ToastKind::Finished),
        _ => None,
    }
}

pub fn notification_toast_for_pane_state_update(
    suppress_active_tab_notifications: bool,
    suppress_completion: bool,
    previous_state: AgentState,
    state: AgentState,
    previous_agent_label: Option<&str>,
    agent_label: Option<&str>,
) -> Option<ToastKind> {
    if suppress_completion || suppress_active_tab_notifications || state == previous_state {
        return None;
    }

    notification_toast_for_state_change_with_agent_labels(
        suppress_active_tab_notifications,
        previous_state,
        state,
        previous_agent_label,
        agent_label,
    )
}

pub(super) fn toast_agent_label(agent_label: &str) -> &str {
    agent_label
}

pub(super) fn toast_event_text(kind: ToastKind) -> &'static str {
    match kind {
        ToastKind::NeedsAttention => "needs attention",
        ToastKind::Finished => "finished",
        ToastKind::UpdateInstalled => "updated",
    }
}

pub(super) fn sound_for_toast_kind(
    kind: ToastKind,
    suppress_active_tab_notifications: bool,
) -> Option<crate::sound::Sound> {
    match kind {
        ToastKind::NeedsAttention => Some(crate::sound::Sound::Request),
        ToastKind::Finished if !suppress_active_tab_notifications => {
            Some(crate::sound::Sound::Done)
        }
        ToastKind::Finished | ToastKind::UpdateInstalled => None,
    }
}

pub fn notification_context(
    ws: &crate::workspace::Workspace,
    workspace_label: &str,
    ws_idx: usize,
    pane_id: PaneId,
) -> String {
    let mut context = format!("{} · {}", workspace_label, ws_idx + 1);
    if ws.tabs.len() > 1 {
        if let Some(tab_idx) = ws.find_tab_index_for_pane(pane_id) {
            if let Some(label) = ws.tab_display_name(tab_idx) {
                context.push_str(&format!(" · {label}"));
            }
        }
    }
    context
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;

    #[test]
    fn notification_context_formats_resolved_workspace_label() {
        let ws = Workspace::test_new("stale");
        let root = ws.tabs[0].root_pane;

        assert_eq!(
            notification_context(&ws, "__herdr_projects__", 0, root),
            "__herdr_projects__ · 1"
        );
    }

    #[test]
    fn waiting_sound_plays_even_in_active_workspace() {
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Blocked),
            Some(crate::sound::Sound::Request)
        );
    }

    #[test]
    fn done_sound_only_plays_in_background() {
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Working, AgentState::Idle),
            Some(crate::sound::Sound::Done)
        );
        assert_eq!(
            notification_sound_for_state_change(true, AgentState::Working, AgentState::Idle),
            None
        );
        assert_eq!(
            notification_sound_for_state_change(false, AgentState::Unknown, AgentState::Idle),
            None
        );
    }

    #[test]
    fn active_tab_suppression_preserves_unknown_focus_behavior() {
        assert!(active_tab_suppresses_notifications(true, None));
        assert!(active_tab_suppresses_notifications(true, Some(true)));
        assert!(!active_tab_suppresses_notifications(true, Some(false)));
        assert!(!active_tab_suppresses_notifications(false, None));
    }
}
