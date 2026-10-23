use std::path::PathBuf;

use super::{
    api_helpers::{pane_agent_status, tab_attention_priority},
    App, Mode,
};
use crate::api::schema::{EventData, EventEnvelope, EventKind};
use crate::{config::NewTerminalCwdConfig, workspace::Workspace};

pub(crate) fn resolve_new_terminal_cwd(
    policy: &NewTerminalCwdConfig,
    follow_cwd: Option<PathBuf>,
) -> PathBuf {
    match policy {
        NewTerminalCwdConfig::Follow => follow_cwd
            .or_else(|| std::env::var_os("HOME").map(PathBuf::from))
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Home => std::env::var_os("HOME")
            .map(PathBuf::from)
            .or_else(|| std::env::current_dir().ok())
            .unwrap_or_else(|| PathBuf::from("/")),
        NewTerminalCwdConfig::Current => {
            std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"))
        }
        NewTerminalCwdConfig::Path(path) => crate::worktree::expand_tilde_path(path),
    }
}

fn composer_info_from_assessment(
    assessment: crate::terminal::ComposerAssessment,
) -> crate::api::schema::ComposerInfo {
    use crate::api::schema::{
        ComposerCursorEvidence as Cursor, ComposerProvenance as Provenance,
        ComposerRegionEvidence as Region, ComposerState as State, ComposerStyleEvidence as Style,
    };
    use crate::terminal::{
        ComposerAssessmentState, ComposerCursorObservation, ComposerInputSource,
        ComposerRegionObservation, ComposerStyleObservation,
    };

    crate::api::schema::ComposerInfo {
        submit_abandoned: false,
        author: match assessment.source {
            Some(ComposerInputSource::Human) => Some(crate::api::schema::ComposerAuthor::Human),
            Some(ComposerInputSource::Api) | Some(ComposerInputSource::AgentPrompt) => {
                Some(crate::api::schema::ComposerAuthor::ApiClient)
            }
            None => None,
        },
        state: match assessment.state {
            ComposerAssessmentState::Empty => State::Empty,
            ComposerAssessmentState::DraftPresent => State::DraftPresent,
            ComposerAssessmentState::Unknown => State::Unknown,
        },
        attempt_id: assessment.attempt_id,
        evidence: crate::api::schema::ComposerEvidence {
            provenance: match assessment.source {
                Some(ComposerInputSource::Human) => Provenance::Human,
                Some(ComposerInputSource::Api) => Provenance::Api,
                Some(ComposerInputSource::AgentPrompt) => Provenance::AgentPrompt,
                None => Provenance::None,
            },
            region: match assessment.visual.region {
                ComposerRegionObservation::Empty => Region::Empty,
                ComposerRegionObservation::Text => Region::Text,
                ComposerRegionObservation::Missing => Region::Missing,
                ComposerRegionObservation::Unavailable => Region::Unavailable,
            },
            cursor: match assessment.visual.cursor {
                ComposerCursorObservation::Draft => Cursor::Draft,
                ComposerCursorObservation::Suggestion => Cursor::Suggestion,
                ComposerCursorObservation::Conflict => Cursor::Conflict,
                ComposerCursorObservation::Unavailable => Cursor::Unavailable,
            },
            style: match assessment.visual.style {
                ComposerStyleObservation::Neutral => Style::Neutral,
                ComposerStyleObservation::Conflict => Style::Conflict,
                ComposerStyleObservation::Unavailable => Style::Unavailable,
            },
            frame_stable: assessment.visual.frame_stable,
        },
    }
}

pub(super) fn launch_cwd_for_terminal(
    terminal_id: &crate::terminal::TerminalId,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &crate::terminal::TerminalRuntimeRegistry,
) -> Option<PathBuf> {
    terminal_runtimes
        .get(terminal_id)
        .and_then(|runtime| runtime.follow_cwd())
        .or_else(|| {
            terminals
                .get(terminal_id)
                .map(|terminal| terminal.cwd.clone())
        })
}

impl App {
    pub(crate) fn record_pane_composer_write(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        source: crate::terminal::ComposerInputSource,
        baseline_content_seq: u64,
        create_if_absent: bool,
        force_new_attempt: bool,
    ) -> Option<String> {
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)?
            .terminal_id(pane_id)?
            .clone();
        self.record_terminal_composer_write(
            &terminal_id,
            source,
            baseline_content_seq,
            create_if_absent,
            force_new_attempt,
        )
    }

    /// Remember the flag a delayed submission will raise if its guard refuses to
    /// write, so `agent get` can report that the prompt never submitted.
    pub(crate) fn record_pane_prompt_submit_watch(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        flag: std::sync::Arc<crate::terminal::PromptSubmitWatch>,
    ) {
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
        else {
            return;
        };
        if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
            // A second prompt inside the delay window would otherwise orphan the
            // first watch, and any abandonment it later raises becomes
            // unobservable — a silent-loss channel in the very mechanism that
            // exists to make loss visible. Displacing an ARMED watch is itself
            // an unknown outcome, so record it rather than dropping it.
            // A retirement belongs to the prompt that earned it. Arming a NEW
            // watch means a new claim exists, so the old verdict must not be
            // applied to it — left sticky, `spent` degenerates into
            // "has this pane ever retired", and every later prompt is stripped
            // of provenance, attempt_id and author at report time. That is the
            // same degeneration family as the dead turn_at_write predicate.
            terminal.prompt_claim_retired = false;
            if let Some(previous) = terminal.prompt_submit_abandoned.replace(flag) {
                // "Unresolved" now means neither outcome was recorded: the
                // watch grew a second flag in #31, so checking abandonment
                // alone would treat a SUBMITTED prompt as displaced-unknown.
                let resolved = previous.abandoned.load(std::sync::atomic::Ordering::SeqCst)
                    || previous.submitted.load(std::sync::atomic::Ordering::SeqCst);
                if !resolved {
                    terminal.displaced_submit_unknown = true;
                }
            }
        }
    }

    pub(crate) fn record_terminal_composer_write(
        &mut self,
        terminal_id: &crate::terminal::TerminalId,
        source: crate::terminal::ComposerInputSource,
        baseline_content_seq: u64,
        create_if_absent: bool,
        force_new_attempt: bool,
    ) -> Option<String> {
        let terminal = self.state.terminals.get_mut(terminal_id)?;
        if terminal.composer_write.is_none() && !create_if_absent {
            return None;
        }
        terminal.is_agent_terminal().then(|| {
            terminal.record_composer_write(source, baseline_content_seq, force_new_attempt)
        })
    }

    pub(super) fn seed_cwd_from_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        self.state
            .workspaces
            .get(ws_idx)?
            .resolved_identity_cwd_from(&self.state.terminals, &self.terminal_runtimes)
    }

    pub(super) fn launch_cwd_for_pane_in_workspace(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<PathBuf> {
        let workspace = self.state.workspaces.get(ws_idx)?;
        let tab = workspace
            .tabs
            .get(workspace.find_tab_index_for_pane(pane_id)?)?;
        launch_cwd_for_terminal(
            tab.terminal_id(pane_id)?,
            &self.state.terminals,
            &self.terminal_runtimes,
        )
    }

    pub(super) fn focused_pane_cwd_in_workspace(&self, ws_idx: usize) -> Option<PathBuf> {
        let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
        self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id)
    }

    pub(super) fn resolve_new_terminal_cwd(&self, follow_cwd: Option<PathBuf>) -> PathBuf {
        resolve_new_terminal_cwd(&self.state.new_terminal_cwd, follow_cwd)
    }

    pub(crate) fn resolved_new_workspace_cwd_from(&self, ws_idx: usize) -> PathBuf {
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)
            .map(crate::workspace::Workspace::active_tab_index);
        self.resolved_new_workspace_cwd_from_tab(ws_idx, tab_idx)
    }

    pub(crate) fn resolved_new_workspace_cwd_from_tab(
        &self,
        ws_idx: usize,
        tab_idx: Option<usize>,
    ) -> PathBuf {
        let follow_cwd = tab_idx
            .and_then(|tab_idx| self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx))
            .map(|tab| tab.layout.focused())
            .and_then(|pane_id| self.launch_cwd_for_pane_in_workspace(ws_idx, pane_id))
            .or_else(|| self.seed_cwd_from_workspace(ws_idx));
        self.resolve_new_terminal_cwd(follow_cwd)
    }

    pub(super) fn workspace_creation_source(&self) -> Option<usize> {
        if self.state.mode == Mode::Navigate
            && self.state.workspaces.get(self.state.selected).is_some()
        {
            return Some(self.state.selected);
        }

        self.state.active.or_else(|| {
            self.state
                .workspaces
                .get(self.state.selected)
                .map(|_| self.state.selected)
        })
    }

    pub(crate) fn create_workspace_with_options(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
    ) -> std::io::Result<usize> {
        self.create_workspace_with_launch_env(initial_cwd, focus, Vec::new())
    }

    pub(crate) fn create_workspace_with_launch_env(
        &mut self,
        initial_cwd: PathBuf,
        focus: bool,
        extra_env: Vec<(String, String)>,
    ) -> std::io::Result<usize> {
        let (rows, cols) = self.state.new_pane_size(crate::ui::NewPanePlacement::Alone);
        let (ws, terminal, runtime) = Workspace::new_with_extra_env(
            initial_cwd,
            rows,
            cols,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
            extra_env,
        )?;
        self.terminal_runtimes.insert(terminal.id.clone(), runtime);
        self.state.terminals.insert(terminal.id.clone(), terminal);
        self.state.workspaces.push(ws);
        let idx = self.state.workspaces.len() - 1;
        self.state
            .remove_alias_shadowed_by_new_pane(self.state.workspaces[idx].tabs[0].root_pane);
        let workspace_id = self.state.workspaces[idx].id.clone();
        let root_pane = self.state.workspaces[idx].tabs[0].root_pane.raw();
        crate::logging::workspace_created(&workspace_id, root_pane);
        if focus || self.state.active.is_none() {
            self.state.switch_workspace(idx);
            self.state.mode = Mode::Terminal;
        }
        self.schedule_session_save();
        Ok(idx)
    }

    pub(super) fn collect_panes_for_workspace(
        &self,
        workspace_id: Option<&str>,
    ) -> Result<Vec<crate::api::schema::PaneInfo>, (String, String)> {
        if let Some(workspace_id) = workspace_id {
            let Some(ws_idx) = self.parse_workspace_id(workspace_id) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            let Some(ws) = self.state.workspaces.get(ws_idx) else {
                return Err((
                    "workspace_not_found".into(),
                    format!("workspace {workspace_id} not found"),
                ));
            };
            Ok(ws
                .tabs
                .iter()
                .flat_map(|tab| tab.layout.pane_ids().into_iter())
                .filter_map(|pane_id| self.pane_info(ws_idx, pane_id))
                .collect())
        } else {
            Ok(self
                .state
                .workspaces
                .iter()
                .enumerate()
                .flat_map(|(ws_idx, ws)| {
                    ws.tabs
                        .iter()
                        .flat_map(|tab| tab.layout.pane_ids().into_iter())
                        .filter_map(move |pane_id| self.pane_info(ws_idx, pane_id))
                })
                .collect())
        }
    }

    pub(super) fn tab_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::TabInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let (agg_state, seen) = tab
            .panes
            .values()
            .filter_map(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| (terminal.state, pane.seen))
            })
            .max_by_key(|(state, seen)| tab_attention_priority(*state, *seen))
            .unwrap_or((crate::detect::AgentState::Unknown, true));
        Some(crate::api::schema::TabInfo {
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            workspace_id: self.public_workspace_id(ws_idx),
            number: tab.number,
            label: ws.tab_display_name(tab_idx)?,
            focused: self.state.active == Some(ws_idx) && ws.active_tab == tab_idx,
            pane_count: tab.panes.len(),
            agent_status: pane_agent_status(agg_state, seen),
            machine_id: None,
            machine_profile_id: None,
            machine_label: None,
        })
    }

    pub(crate) fn emit_workspace_open_events(&mut self, ws_idx: usize) {
        let workspace_info = self.workspace_info(ws_idx);
        let Some(tab) = self.tab_info(ws_idx, 0) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, 0) else {
            return;
        };
        self.emit_event(EventEnvelope {
            event: EventKind::WorkspaceCreated,
            data: EventData::WorkspaceCreated {
                workspace: workspace_info,
            },
        });
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, 0);
    }

    pub(crate) fn emit_tab_created_events(&mut self, ws_idx: usize, tab_idx: usize) {
        let Some(tab) = self.tab_info(ws_idx, tab_idx) else {
            return;
        };
        let Some(root_pane) = self.root_pane_info(ws_idx, tab_idx) else {
            return;
        };
        self.emit_tab_and_pane_created_events(tab, root_pane);
        self.emit_layout_updated_event(ws_idx, tab_idx);
    }

    fn emit_tab_and_pane_created_events(
        &mut self,
        tab: crate::api::schema::TabInfo,
        root_pane: crate::api::schema::PaneInfo,
    ) {
        self.emit_event(EventEnvelope {
            event: EventKind::TabCreated,
            data: EventData::TabCreated { tab },
        });
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: root_pane },
        });
    }

    pub(super) fn workspace_created_result(
        &self,
        ws_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::WorkspaceCreated {
            workspace: self.workspace_info(ws_idx),
            tab: self.tab_info(ws_idx, 0)?,
            root_pane: self.root_pane_info(ws_idx, 0)?,
        })
    }

    pub(super) fn tab_created_result(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::ResponseResult> {
        Some(crate::api::schema::ResponseResult::TabCreated {
            tab: self.tab_info(ws_idx, tab_idx)?,
            root_pane: self.root_pane_info(ws_idx, tab_idx)?,
        })
    }

    pub(super) fn root_pane_info(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        self.pane_info(ws_idx, tab.root_pane)
    }

    pub(super) fn pane_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let mut pane = self.pane_metadata(ws_idx, pane_id)?;
        pane.scroll = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| runtime.scroll_metrics())
            .map(|metrics| crate::api::schema::PaneScrollInfo {
                offset_from_bottom: metrics.offset_from_bottom as u64,
                max_offset_from_bottom: metrics.max_offset_from_bottom as u64,
                viewport_rows: metrics.viewport_rows as u64,
            });
        pane.alternate_screen = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| runtime.active_screen())
            .map(|screen| screen == crate::ghostty::ActiveScreen::Alternate)
            .unwrap_or(false);
        Some(pane)
    }

    pub(super) fn pane_metadata(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::PaneInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane.attached_terminal_id)?;
        let tab_idx = ws.find_tab_index_for_pane(pane_id)?;
        let focused = self.state.active == Some(ws_idx)
            && ws.active_tab == tab_idx
            && ws
                .focused_pane_id()
                .is_some_and(|focused| focused == pane_id);
        let presentation = terminal.effective_presentation();
        let composer = self
            .state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
            .and_then(|runtime| {
                terminal.effective_known_agent().map(|agent| {
                    let (frame, content_seq) = runtime.composer_frame();
                    crate::terminal::assess_composer(
                        terminal.composer_write.as_ref(),
                        crate::detect::manifest::composer_visual_observation(
                            agent,
                            &frame.screen,
                            frame.cursor,
                            frame.frame_stable,
                            content_seq,
                        ),
                    )
                })
            })
            .map(composer_info_from_assessment)
            .map(|mut composer| {
                use std::sync::atomic::Ordering;
                let watch = terminal.prompt_submit_abandoned.as_ref();
                // Union of both lines: master's displaced-watch signal (#32 F2)
                // OR this watch's own abandonment.
                composer.submit_abandoned = terminal.displaced_submit_unknown
                    || watch.is_some_and(|watch| watch.abandoned.load(Ordering::SeqCst));

                // #31: a submitted prompt empties the composer, so it cannot be
                // the author of text seen afterwards. Without this, keyboard
                // input inherits the last prompter's identity and a stale
                // attempt_id — reported confidently, and wrongly.
                // Retire the prompt's claim only when the key was written AND a
                // turn has COMPLETED since. `turn` increments in
                // record_completed_turn_at (src/terminal/state.rs:388), so it
                // counts completions, not starts — an earlier comment here
                // claimed "a turn has begun" and was wrong.
                //
                // Completion is a conservative proxy: it lags, so a prompt that
                // submitted but whose turn is still running keeps its claim for
                // longer than strictly necessary. That errs toward RETAINING
                // attribution, which is the safe direction — the failure this
                // issue exists to fix is claiming authorship we cannot support,
                // not holding a true claim slightly too long. A stranded prompt
                // never completes a turn, so it keeps its draft forever, which
                // is correct.
                // Set by record_completed_turn_at when a SUBMITTED watch was
                // discarded. Reading it here cannot race the clear, because the
                // clear is what produces it.
                let spent = terminal.prompt_claim_retired;
                if spent
                    && composer.evidence.provenance
                        == crate::api::schema::ComposerProvenance::AgentPrompt
                {
                    composer.evidence.provenance = crate::api::schema::ComposerProvenance::None;
                    composer.attempt_id = None;
                    composer.author = None;
                }
                composer
            })
            .unwrap_or_default();
        Some(crate::api::schema::PaneInfo {
            pane_id: self.public_pane_id(ws_idx, pane_id)?,
            terminal_id: terminal.id.to_string(),
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            focused,
            cwd: ws.tabs[tab_idx]
                .cwd_for_pane(pane_id, &self.state.terminals, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            foreground_cwd: ws.tabs[tab_idx]
                .foreground_cwd_for_pane(pane_id, &self.terminal_runtimes)
                .map(|cwd| cwd.display().to_string()),
            restore_error: terminal.restore_error.clone(),
            label: terminal.manual_label.clone(),
            agent: terminal.effective_agent_label().map(str::to_string),
            title: presentation.title,
            terminal_title: terminal.terminal_title.clone(),
            terminal_title_stripped: terminal.terminal_title_stripped(),
            display_agent: presentation.display_agent,
            agent_status: pane_agent_status(terminal.state, pane.seen),
            input_pending: terminal.input_pending,
            input_prompt_kind: terminal.input_prompt_kind,
            composer,
            state_labels: presentation.state_labels,
            tokens: terminal.metadata_tokens.values(),
            agent_session: terminal_agent_session_info(terminal),
            last_completed_turn: terminal.last_completed_turn().map(|record| {
                crate::api::schema::LastCompletedTurn {
                    turn: record.turn,
                    turn_epoch: record.turn_epoch,
                    completed_unix_ms: record.completed_unix_ms,
                }
            }),
            turn: terminal.is_agent_terminal().then_some(terminal.turn),
            turn_epoch: terminal.is_agent_terminal().then_some(terminal.turn_epoch),
            scroll: None,
            alternate_screen: false,
            revision: terminal.revision,
        })
    }

    pub(super) fn lookup_runtime(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<(&crate::terminal::TerminalRuntime, String)> {
        let runtime =
            self.state
                .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)?;
        Some((runtime, self.public_workspace_id(ws_idx)))
    }

    pub(super) fn lookup_runtime_sender(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<&crate::terminal::TerminalRuntime> {
        self.state
            .runtime_for_pane_in_workspace(&self.terminal_runtimes, ws_idx, pane_id)
    }

    pub(super) fn workspace_info(&self, index: usize) -> crate::api::schema::WorkspaceInfo {
        let ws = &self.state.workspaces[index];
        let (agg_state, seen) = ws.aggregate_state(&self.state.terminals);
        crate::api::schema::WorkspaceInfo {
            workspace_id: self.public_workspace_id(index),
            number: index + 1,
            label: ws.display_name_from(&self.state.terminals, &self.terminal_runtimes),
            focused: self.state.active == Some(index),
            pane_count: ws.public_pane_numbers.len(),
            tab_count: ws.tabs.len(),
            active_tab_id: self.public_tab_id(index, ws.active_tab).unwrap_or_else(|| {
                crate::workspace::public_tab_id_for_number(&ws.id, ws.active_tab + 1)
            }),
            agent_status: pane_agent_status(agg_state, seen),
            tokens: ws.metadata_tokens.values(),
            worktree: ws
                .worktree_space()
                .map(|space| crate::api::schema::WorkspaceWorktreeInfo {
                    repo_key: space.key.clone(),
                    repo_name: space.label.clone(),
                    repo_root: space.repo_root.display().to_string(),
                    checkout_path: space.checkout_path.display().to_string(),
                    is_linked_worktree: space.is_linked_worktree,
                }),
            machine_id: None,
            machine_profile_id: None,
            machine_label: None,
            reachability: None,
            last_known_status: None,
        }
    }
}

pub(super) fn terminal_agent_session_info(
    terminal: &crate::terminal::TerminalState,
) -> Option<crate::api::schema::AgentSessionInfo> {
    if let Some(authority) = terminal.hook_authority.as_ref() {
        if let Some(session_ref) = authority.session_ref.as_ref() {
            return Some(crate::api::schema::AgentSessionInfo {
                source: authority.source.clone(),
                agent: authority.agent_label.clone(),
                kind: session_ref.kind,
                value: session_ref.value.clone(),
            });
        }
    }

    terminal
        .persisted_agent_session
        .as_ref()
        .map(|session| crate::api::schema::AgentSessionInfo {
            source: session.source.clone(),
            agent: session.agent.clone(),
            kind: session.session_ref.kind,
            value: session.session_ref.value.clone(),
        })
}
