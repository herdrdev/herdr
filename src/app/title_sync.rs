use std::path::PathBuf;

use super::App;
use crate::title_sync::{
    agent_identity, has_agent_identity, rename_decision, sanitize_title, session_key, AgentSession,
    Ownership, OwnershipMutation, PaneInput, RenameDecision, ResolvedPane,
};

impl App {
    pub(crate) fn title_sync_inputs(&self) -> Vec<PaneInput> {
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, workspace)| {
                workspace.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.title_sync_input(ws_idx, pane_id))
                })
            })
            .collect()
    }

    fn title_sync_input(&self, ws_idx: usize, pane_id: crate::layout::PaneId) -> Option<PaneInput> {
        let info = self.pane_info(ws_idx, pane_id)?;
        let shell_pid = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(crate::terminal::TerminalRuntime::child_pid);
        Some(PaneInput {
            pane_id: info.pane_id,
            agent: info.agent,
            agent_session: info.agent_session.map(|session| AgentSession {
                agent: Some(session.agent),
                kind: Some(
                    match session.kind {
                        crate::agent_resume::AgentSessionRefKind::Id => "id",
                        crate::agent_resume::AgentSessionRefKind::Path => "path",
                    }
                    .into(),
                ),
                value: Some(session.value),
            }),
            cwd: info.cwd.map(PathBuf::from),
            foreground_cwd: info.foreground_cwd.map(PathBuf::from),
            label: info.label,
            terminal_title: info.terminal_title,
            terminal_title_stripped: info.terminal_title_stripped,
            shell_pid,
            foreground_processes: Vec::new(),
        })
    }

    pub(crate) fn mutate_pane_label(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        label: Option<String>,
    ) -> Option<crate::api::schema::PaneInfo> {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)?
            .terminal_id(pane_id)?
            .clone();
        let label = label.and_then(|label| {
            let label = label.trim().to_string();
            (!label.is_empty()).then_some(label)
        });
        let changed = {
            let terminal = self.state.terminals.get_mut(&terminal_id)?;
            if terminal.manual_label == label {
                false
            } else {
                match label {
                    Some(label) => terminal.set_manual_label(label),
                    None => terminal.clear_manual_label(),
                }
                true
            }
        };
        if changed {
            self.state.mark_session_dirty();
            self.emit_pane_updated(ws_idx, pane_id);
        }
        self.pane_info(ws_idx, pane_id)
    }

    pub(crate) fn apply_title_sync_results(
        &mut self,
        panes: Vec<ResolvedPane>,
    ) -> Vec<OwnershipMutation> {
        let mut mutations = Vec::new();
        for resolved in panes {
            let Some((ws_idx, pane_id)) = self.parse_pane_id(&resolved.pane_id) else {
                continue;
            };
            let Some(current) = self.title_sync_input(ws_idx, pane_id) else {
                continue;
            };
            if session_key(&current) != resolved.expected_session
                || agent_identity(&current) != resolved.expected_agent.as_deref()
                || sanitize_title(current.label.as_deref(), 200) != resolved.expected_label
            {
                continue;
            }

            if !resolved.had_agent || !has_agent_identity(&current) {
                if resolved.previous.as_ref().map(|state| state.title.as_str())
                    == sanitize_title(current.label.as_deref(), 200).as_deref()
                {
                    let _ = self.mutate_pane_label(ws_idx, pane_id, None);
                }
                mutations.push(OwnershipMutation {
                    pane_id: resolved.pane_id,
                    state: None,
                });
                continue;
            }

            match rename_decision(
                &current,
                resolved.desired_title.as_deref(),
                resolved.previous.as_ref(),
            ) {
                RenameDecision::Manual => mutations.push(OwnershipMutation {
                    pane_id: resolved.pane_id,
                    state: None,
                }),
                RenameDecision::Clear => {
                    let _ = self.mutate_pane_label(ws_idx, pane_id, None);
                    mutations.push(OwnershipMutation {
                        pane_id: resolved.pane_id,
                        state: None,
                    });
                }
                RenameDecision::Rename => {
                    let Some(title) = resolved.desired_title else {
                        continue;
                    };
                    if self
                        .mutate_pane_label(ws_idx, pane_id, Some(title.clone()))
                        .is_some()
                    {
                        mutations.push(OwnershipMutation {
                            pane_id: resolved.pane_id,
                            state: Some(Ownership {
                                session: session_key(&current),
                                title,
                            }),
                        });
                    }
                }
                RenameDecision::Noop => {
                    if let (Some(title), Some(previous)) =
                        (resolved.desired_title, resolved.previous)
                    {
                        if previous.title == current.label.as_deref().unwrap_or_default() {
                            mutations.push(OwnershipMutation {
                                pane_id: resolved.pane_id,
                                state: Some(Ownership {
                                    session: session_key(&current),
                                    title,
                                }),
                            });
                        }
                    }
                }
            }
        }
        mutations
    }
}
