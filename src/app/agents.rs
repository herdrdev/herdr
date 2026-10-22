use std::time::{Duration, Instant};

use bytes::Bytes;

use super::{terminal_targets::TerminalTargetError, App};
use crate::api::schema::AgentStartParams;

const DEFAULT_AGENT_START_TIMEOUT: Duration = Duration::from_secs(30);
pub(crate) const MAX_AGENT_START_TIMEOUT: Duration = Duration::from_secs(300);
pub(crate) const AGENT_START_SETTLE_DELAY: Duration = Duration::from_secs(3);
const INVALID_AGENT_TIMEOUT_MESSAGE: &str =
    "agent start timeout must be greater than 3000ms and at most 300000ms";
const INVALID_AGENT_NAME_MESSAGE: &str = "agent name must start with a lowercase letter and contain only lowercase letters, digits, '-' or '_' (1-32 characters)";

fn valid_agent_name(name: &str) -> bool {
    let mut chars = name.chars();
    matches!(chars.next(), Some('a'..='z'))
        && name.len() <= 32
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || matches!(ch, '-' | '_'))
}

impl App {
    pub(super) fn collect_agent_infos(&self) -> Vec<crate::api::schema::AgentInfo> {
        // Live agents derived from panes, then the paneless archived store. The
        // archived entries bypass the `is_agent_terminal` pane gate (they have no
        // pane) and carry the `archived` block that marks them; older clients that
        // ignore the field see them as ordinary idle agents.
        self.state
            .workspaces
            .iter()
            .enumerate()
            .flat_map(|(ws_idx, ws)| {
                ws.tabs.iter().flat_map(move |tab| {
                    tab.layout
                        .pane_ids()
                        .into_iter()
                        .filter_map(move |pane_id| self.agent_info(ws_idx, pane_id))
                })
            })
            .chain(self.state.archived_agents.iter().map(archived_agent_info))
            .collect()
    }

    pub(super) fn reconcile_managed_agent_target(&mut self, target: &str) {
        let Ok(resolved) = self.resolve_agent_target(target) else {
            return;
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return;
        };
        let changed = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .is_some_and(|terminal| terminal.reconcile_managed_agent_at(Instant::now(), false));
        if changed {
            self.state.mark_session_dirty();
            self.schedule_session_save();
            self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        }
    }

    pub(super) fn agent_info_for_target(
        &self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn focus_agent_target(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, TerminalTargetError> {
        let resolved = self.resolve_agent_target(target)?;
        self.state
            .focus_pane_in_workspace(resolved.ws_idx, resolved.pane_id);
        self.state.mark_active_tab_seen();
        self.state.mode = crate::app::Mode::Terminal;
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| TerminalTargetError::NotFound {
                target: target.to_string(),
            })
    }

    pub(super) fn rename_agent_target(
        &mut self,
        target: &str,
        name: Option<String>,
    ) -> Result<crate::api::schema::AgentInfo, AgentRenameError> {
        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentRenameError::Target)?;
        let normalized_name = match name {
            Some(name) if valid_agent_name(&name) => Some(name),
            Some(_) => return Err(AgentRenameError::InvalidName),
            None => None,
        };

        if let Some(name) = normalized_name.as_deref() {
            let conflicts = self.agent_name_conflicts(name, &resolved.terminal_id);
            if !conflicts.is_empty() {
                return Err(AgentRenameError::DuplicateName {
                    name: name.to_string(),
                    candidates: conflicts,
                });
            }
        }

        let Some(terminal) = self
            .state
            .terminals
            .values_mut()
            .find(|terminal| terminal.id.to_string() == resolved.terminal_id)
        else {
            return Err(AgentRenameError::Target(TerminalTargetError::NotFound {
                target: target.to_string(),
            }));
        };
        if terminal.managed_agent_launch_pending() {
            return Err(AgentRenameError::PendingLaunch);
        }
        if terminal.effective_agent_label().is_none() {
            return Err(AgentRenameError::NotAgent);
        }
        match normalized_name {
            Some(name) => terminal.set_agent_name(name),
            None => terminal.clear_agent_name(),
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
        self.emit_pane_updated(resolved.ws_idx, resolved.pane_id);
        self.agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or_else(|| {
                AgentRenameError::Target(TerminalTargetError::NotFound {
                    target: target.to_string(),
                })
            })
    }

    /// Archive an agent (issue #173): release its pane but preserve the session
    /// so it can be resumed later. Record-then-release — the resume identity is
    /// captured into `AppState.archived_agents` FIRST, then the process is
    /// released and the now-empty pane removed. Idempotent: archiving an
    /// already-archived name/terminal id is a no-op that returns the record.
    pub(super) fn archive_agent_target(
        &mut self,
        target: &str,
        reason: Option<String>,
        by: String,
        parked_work: Vec<serde_json::Value>,
        force: bool,
        at: String,
    ) -> Result<crate::api::schema::AgentInfo, AgentArchiveError> {
        // Idempotent no-op if already archived.
        if let Some(existing) = self
            .state
            .archived_agents
            .iter()
            .find(|record| archived_matches_target(record, target))
        {
            return Ok(archived_agent_info(existing));
        }

        let resolved = self
            .resolve_agent_target(target)
            .map_err(AgentArchiveError::Target)?;

        let info = self
            .agent_info(resolved.ws_idx, resolved.pane_id)
            .ok_or(AgentArchiveError::NotAgent)?;
        if !force && matches!(info.agent_status, crate::api::schema::AgentStatus::Working) {
            return Err(AgentArchiveError::Working);
        }

        let Some(terminal_id) = self
            .state
            .workspaces
            .get(resolved.ws_idx)
            .and_then(|workspace| workspace.terminal_id(resolved.pane_id))
            .cloned()
        else {
            return Err(AgentArchiveError::NotAgent);
        };

        // Capture the resume identity BEFORE anything is released, so dropping the
        // TerminalState below loses nothing.
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or(AgentArchiveError::NotAgent)?;
        let Some((source, agent, session_ref)) = Self::terminal_resume_source(terminal) else {
            return Err(AgentArchiveError::NoResumableSession);
        };
        let name = terminal.agent_name.clone();
        // The pane's user-facing label, captured so an unarchive can put it back.
        // It lives on the TERMINAL, but it identifies the PANE, and the pane is about
        // to be destroyed — so if it is not taken here it cannot be recovered at all.
        // Fleet tooling binds a role to its pane by this label, which is why losing it
        // silently unhooks a restored agent from the channel that addresses it.
        let pane_label = terminal.manual_label.clone();
        let cwd = terminal.cwd.clone();
        let occupant_generation = terminal.occupant_generation;
        let kind = terminal
            .managed_agent_kind()
            .map(|agent| crate::detect::agent_label(agent).to_string())
            .unwrap_or_else(|| agent.clone());

        self.state
            .archived_agents
            .push(crate::persist::ArchivedAgentSnapshot {
                name,
                kind,
                terminal_id: terminal_id.to_string(),
                agent_session: crate::persist::PaneAgentSessionSnapshot {
                    source,
                    agent,
                    kind: session_ref.kind,
                    value: session_ref.value,
                },
                cwd,
                occupant_generation,
                archived: crate::persist::ArchivedAgentMeta { at, by, reason },
                parked_work,
                // Where it came from. `info` was built above from this very pane, so
                // both ids are already resolved public ids — the same strings
                // `parse_tab_id` / `parse_workspace_id` take back.
                origin_workspace_id: Some(info.workspace_id.clone()),
                origin_tab_id: Some(info.tab_id.clone()),
                pane_label,
            });

        // Release the pane's process, then remove the now-empty pane. The session
        // was captured above, so this is a safe, non-`release_agent_with_mutation`
        // teardown.
        self.shutdown_terminal_runtime(terminal_id.clone());
        let should_close_workspace = self
            .state
            .workspaces
            .get_mut(resolved.ws_idx)
            .is_some_and(|workspace| workspace.close_pane(resolved.pane_id));
        self.state.remove_plugin_pane_records([resolved.pane_id]);
        if should_close_workspace {
            self.state.selected = resolved.ws_idx;
            self.state.close_selected_workspace();
        } else {
            self.state.remove_unattached_terminal_ids([terminal_id]);
        }
        self.shutdown_detached_terminal_runtimes();

        self.state.mark_session_dirty();
        self.schedule_session_save();

        let record = self
            .state
            .archived_agents
            .last()
            .ok_or(AgentArchiveError::NotAgent)?;
        Ok(archived_agent_info(record))
    }

    /// Unarchive an agent (issue #173): resume the stored session into a fresh
    /// pane, preserving the agent's terminal identity. Mirrors the restore path —
    /// a new terminal carries the resume plan and no runtime; the pending-resume
    /// loop spawns the shell and relaunches with the agent's resume command.
    ///
    /// When `fresh` is set, the resume plan is skipped entirely: the preserved
    /// terminal identity comes back hosting a clean agent of the archived
    /// `kind` in the archived `cwd`. This is the operator's escape hatch when
    /// the stored session is gone or unwanted.
    pub(super) fn unarchive_agent_target(
        &mut self,
        target: &str,
        fresh: bool,
    ) -> Result<crate::api::schema::AgentInfo, AgentUnarchiveError> {
        let Some(index) = self
            .state
            .archived_agents
            .iter()
            .position(|record| archived_matches_target(record, target))
        else {
            return Err(AgentUnarchiveError::NotFound);
        };

        // Build the resume plan before removing the record, so a plan failure —
        // or a lost session file — leaves the archive intact. `--fresh` skips
        // this entirely and starts a clean agent instead.
        let resume = if fresh {
            None
        } else {
            let record = &self.state.archived_agents[index];
            let Some(persisted) = crate::agent_resume::session_ref_from_snapshot(
                &record.agent_session.source,
                &record.agent_session.agent,
                record.agent_session.kind,
                &record.agent_session.value,
            ) else {
                return Err(AgentUnarchiveError::NoResumablePlan);
            };
            let Some(plan) = crate::agent_resume::plan(
                &record.agent_session.source,
                &record.agent_session.agent,
                &persisted.session_ref,
            ) else {
                return Err(AgentUnarchiveError::NoResumablePlan);
            };

            // Fail-loud existence probe for PATH-kind session refs only (pi/omp):
            // if the archived session file is gone, resuming it would silently
            // start a brand-new session, so refuse and point the operator at
            // `--fresh`. Runs after the plan is built and before the record is
            // removed, so a miss leaves the archive intact — mirroring the
            // plan-failure contract above.
            //
            // ID-kind refs (claude/codex/kimi and the other id-kind harnesses)
            // are not probed here — resume is attempted exactly as before.
            // H3: add a per-harness id->session-path locator (claude
            // ~/.claude/projects/<slug>/<id>.jsonl, codex/kimi dirs) so id-kind
            // harnesses also get this fail-loud probe.
            if persisted.session_ref.kind == crate::agent_resume::AgentSessionRefKind::Path
                && !std::path::Path::new(&persisted.session_ref.value).exists()
            {
                return Err(AgentUnarchiveError::SessionLost);
            }

            Some((persisted, plan))
        };

        // REFUSE A SECOND PROCESS ON ONE SESSION.
        //
        // Nothing else stops this: resuming a session a live agent already holds puts
        // two harness processes on one transcript, and it has happened on a real box —
        // one agent started as a workaround while its twin was archived, then the twin
        // was unarchived. Both then wrote org acknowledgements that could not be
        // attributed to either process.
        //
        // Checked BEFORE the record is removed, so a refusal leaves the archive intact
        // and retryable — the same fail-loud contract as the plan and session-file
        // checks above. Skipped for `fresh`, which resumes nothing and so cannot
        // duplicate anything.
        if let Some((_, plan)) = resume.as_ref() {
            let key = crate::agent_resume::dedupe_key(
                &self.state.archived_agents[index].agent_session.source,
                &plan.agent,
                &crate::agent_resume::AgentSessionRef {
                    kind: self.state.archived_agents[index].agent_session.kind,
                    value: self.state.archived_agents[index]
                        .agent_session
                        .value
                        .clone(),
                },
            );
            if let Some(pane) = self.live_pane_holding_session(&key) {
                return Err(AgentUnarchiveError::SessionInUse { pane });
            }
        }

        let record = self.state.archived_agents.remove(index);
        let terminal_id = crate::terminal::TerminalId::from_persisted(record.terminal_id.clone());
        let pane_id = crate::layout::PaneId::alloc();
        let initial_agent = crate::detect::parse_agent_label(match resume.as_ref() {
            Some((_, plan)) => plan.agent.as_str(),
            None => record.kind.as_str(),
        });

        let mut terminal =
            crate::terminal::TerminalState::new(terminal_id.clone(), record.cwd.clone())
                .with_occupant_generation(record.occupant_generation);
        if let Some((persisted, plan)) = resume {
            terminal = terminal.with_pending_agent_resume_plan(plan);
            terminal.set_persisted_agent_session(persisted);
        }
        match (
            record.name.clone(),
            crate::detect::parse_agent_label(&record.kind),
        ) {
            (Some(name), Some(agent_kind)) => terminal.restore_managed_agent(name, agent_kind),
            (Some(name), None) => terminal.set_agent_name(name),
            (None, _) => {}
        }
        if let Some(agent) = initial_agent {
            let _ = terminal.set_detected_state_with_screen_signals_at(
                Some(agent),
                crate::detect::AgentState::Idle,
                false,
                false,
                false,
                false,
                Instant::now(),
            );
        }
        self.state.terminals.insert(terminal_id.clone(), terminal);

        // Re-apply the pane label BEFORE the pane is placed, so whichever branch below
        // takes it, the restored pane is addressable by the name it had. Fleet tooling
        // binds a role to its pane by this label; a restored agent without one looks
        // healthy and is unreachable on the channel that addresses it.
        if let Some(label) = record.pane_label.clone() {
            if let Some(terminal) = self.state.terminals.get_mut(&terminal_id) {
                terminal.set_manual_label(label);
            }
        }

        let moved = crate::workspace::MovedPane {
            pane_id,
            pane_state: crate::pane::PaneState::new(terminal_id.clone()),
        };

        // PUT IT BACK WHERE IT CAME FROM. Three tiers, most specific first, mirroring
        // `recover_failed_pane_move` — an archive can outlive its tab or its whole
        // workspace, so each tier must degrade rather than fail.
        let ws_idx = self.restore_archived_pane(&record, moved)?;
        self.state.remove_alias_shadowed_by_new_pane(pane_id);
        self.state.mode = crate::app::Mode::Terminal;

        // SPAWN THE RUNTIME NOW — an unarchived pane must never be advertised without
        // one.
        //
        // Everything above rebuilds pure state: a terminal, a pane, a workspace, an
        // armed resume plan. None of that is a live PTY, and the deferred launcher that
        // would have spawned one only collects candidates when `view.terminal_area` is
        // non-zero (`start_pending_agent_resumes`) — geometry a HEADLESS daemon never
        // has. So on a server with no rendering client the PTY was never spawned, while
        // `agent.list` and `pane.list` (which read state and treat the runtime as
        // optional) happily advertised the pane. Every path that needs a real terminal
        // — `pane.read`, `pane.stream`, `agent.read`, `pane.set_pty_size` — then
        // answered `pane_not_found` for a pane the same daemon had just listed, and a
        // client that opened it retried forever.
        //
        // Spawn eagerly on the same headless-safe terms `agent.restart` already uses:
        // `new_pane_size` (falls back to a headless size) and
        // `allow_empty_theme = true`, because an unarchive is an explicit operator
        // action that must not wait on a host theme a headless daemon may never report.
        let (rows, cols) = self
            .state
            .new_pane_size(crate::ui::NewPanePlacement::Existing {
                ws_idx,
                pane: pane_id,
            });
        if !self.start_pending_agent_resume_for_terminal(&terminal_id, rows, cols, true) {
            // No plan to resume (`fresh`), or the resume launcher declined. The pane
            // still has to be usable, so give it a plain shell in the archived cwd
            // rather than leave the dead pane this whole block exists to prevent.
            self.spawn_plain_runtime_for_unarchived_pane(ws_idx, pane_id, &terminal_id, rows, cols);
        }

        self.state.mark_session_dirty();
        self.schedule_session_save();

        self.agent_info(ws_idx, pane_id)
            .ok_or(AgentUnarchiveError::NoResumablePlan)
    }

    /// Forget an archived agent (issue #291): drop its record from
    /// `AppState.archived_agents` and persist that, returning the record as
    /// `agent.list` rendered it. Only herdr's resume pointer goes; the harness
    /// transcript and session files on disk are deliberately left alone.
    ///
    /// Resolves exactly like `unarchive_agent_target` — the first archived record
    /// whose name or terminal id matches — so the two never disagree about which
    /// record a target names. A target that only matches a LIVE agent is refused:
    /// forgetting is for archived records, and a live agent has to be archived first.
    pub(super) fn forget_archived_agent(
        &mut self,
        target: &str,
    ) -> Result<crate::api::schema::AgentInfo, AgentForgetError> {
        let Some(index) = self
            .state
            .archived_agents
            .iter()
            .position(|record| archived_matches_target(record, target))
        else {
            let live = matches!(
                self.resolve_agent_target(target),
                Ok(_) | Err(TerminalTargetError::Ambiguous { .. })
            ) || self
                .state
                .terminals
                .get(target)
                .is_some_and(|terminal| terminal.is_agent_terminal());
            return Err(if live {
                AgentForgetError::Live
            } else {
                AgentForgetError::NotFound
            });
        };

        let record = self.state.archived_agents.remove(index);
        self.state.mark_session_dirty();
        self.schedule_session_save();

        Ok(archived_agent_info(&record))
    }

    /// The public id of a LIVE pane already running the given session, if any.
    ///
    /// Walks panes rather than `state.terminals`, because a terminal can linger detached
    /// after its pane is gone — matching one of those would refuse a legitimate
    /// unarchive, which is the more damaging direction to be wrong in.
    ///
    /// Identity is `agent_resume::dedupe_key`, the same key the restore path already
    /// uses to stop two panes launching one native session, so the two guards agree on
    /// what "the same session" means.
    fn live_pane_holding_session(&self, key: &str) -> Option<String> {
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in workspace.tabs.iter() {
                for pane_id in tab.layout.pane_ids() {
                    let Some(pane) = tab.panes.get(&pane_id) else {
                        continue;
                    };
                    let Some(terminal) = self.state.terminals.get(&pane.attached_terminal_id)
                    else {
                        continue;
                    };
                    if !terminal.is_agent_terminal() {
                        continue;
                    }
                    let Some((source, agent, session_ref)) = Self::terminal_resume_source(terminal)
                    else {
                        continue;
                    };
                    if crate::agent_resume::dedupe_key(&source, &agent, &session_ref) == key {
                        return self
                            .public_pane_id(ws_idx, pane_id)
                            .or_else(|| Some(pane_id.raw().to_string()));
                    }
                }
            }
        }
        None
    }

    /// Put an unarchived pane back where it came from, degrading in three tiers.
    ///
    /// An archive can outlive its tab and even its whole workspace, so each tier falls
    /// through rather than failing:
    ///   1. ORIGINAL TAB still exists -> insert alongside its panes.
    ///   2. ORIGINAL WORKSPACE exists but the tab is gone -> new tab in that workspace.
    ///   3. Neither exists (or the record predates this field) -> a new workspace, which
    ///      is exactly the old behaviour, so nothing regresses for old snapshots.
    ///
    /// Returns the workspace index the pane landed in, and focuses it.
    ///
    /// Reuses the same helpers `pane.move` and its recovery path use — notably
    /// `insert_moved_pane_into_tab`, which registers the public pane number that would
    /// otherwise have to be invented here.
    fn restore_archived_pane(
        &mut self,
        record: &crate::persist::ArchivedAgentSnapshot,
        moved: crate::workspace::MovedPane,
    ) -> Result<usize, AgentUnarchiveError> {
        let pane_id = moved.pane_id;

        // Tier 1: the original tab. `parse_tab_id` returns None if EITHER the workspace
        // or the tab is gone, so a hit here means both are live.
        if let Some((ws_idx, tab_idx)) = record
            .origin_tab_id
            .as_deref()
            .and_then(|tab_id| self.parse_tab_id(tab_id))
        {
            let target = self
                .state
                .workspaces
                .get(ws_idx)
                .and_then(|workspace| workspace.tabs.get(tab_idx))
                .map(|tab| tab.root_pane);
            if let Some(target) = target {
                let moved = match self.state.workspaces[ws_idx].insert_moved_pane_into_tab(
                    tab_idx,
                    target,
                    moved,
                    ratatui::layout::Direction::Horizontal,
                    0.5,
                    true,
                ) {
                    Ok(_) => {
                        self.state.switch_workspace_tab(ws_idx, tab_idx);
                        self.emit_restored_pane_created(ws_idx, pane_id);
                        self.emit_layout_updated_event(ws_idx, tab_idx);
                        return Ok(ws_idx);
                    }
                    // The layout rejected the target pane; fall through with the pane
                    // handed back intact rather than losing it.
                    Err(moved) => moved,
                };
                return self.restore_into_new_workspace(record, moved);
            }
        }

        // Tier 2: the workspace survived, the tab did not.
        if let Some(ws_idx) = record
            .origin_workspace_id
            .as_deref()
            .and_then(|ws_id| self.parse_workspace_id(ws_id))
        {
            let tab_idx = self.state.workspaces[ws_idx].create_tab_from_existing_pane(
                moved,
                record.name.clone(),
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.emit_tab_created_events(ws_idx, tab_idx);
            self.emit_restored_pane_created(ws_idx, pane_id);
            return Ok(ws_idx);
        }

        // Tier 3: nothing to go back to.
        self.restore_into_new_workspace(record, moved)
    }

    /// Announce a restored pane the same way `pane.split` announces a new one, so
    /// clients learn about it. The new-workspace tier does not need this — its
    /// `emit_workspace_open_events` already covers the pane.
    fn emit_restored_pane_created(&mut self, ws_idx: usize, pane_id: crate::layout::PaneId) {
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return;
        };
        self.emit_event(crate::api::schema::EventEnvelope {
            event: crate::api::schema::EventKind::PaneCreated,
            data: crate::api::schema::EventData::PaneCreated { pane },
        });
    }

    /// The pre-existing restore behaviour, kept as the last tier: a brand-new workspace
    /// for the restored pane. Reached when the origin is gone, or when the record was
    /// written before the origin was captured at all.
    fn restore_into_new_workspace(
        &mut self,
        record: &crate::persist::ArchivedAgentSnapshot,
        moved: crate::workspace::MovedPane,
    ) -> Result<usize, AgentUnarchiveError> {
        let workspace = crate::workspace::Workspace::from_existing_pane(
            record.name.clone(),
            None,
            record.cwd.clone(),
            moved,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        );
        self.state.workspaces.push(workspace);
        let ws_idx = self.state.workspaces.len() - 1;
        self.state.switch_workspace(ws_idx);
        self.emit_workspace_open_events(ws_idx);
        Ok(ws_idx)
    }

    /// Last-resort shell for a pane that unarchived without a resume plan, so the pane
    /// is openable instead of dead.
    ///
    /// Deliberately NOT `respawn_shell_for_launch_pane`: that path calls
    /// `clear_agent_runtime_identity_after_respawn`, which is right when a launch
    /// command exited and wrong here — preserving the archived agent's identity is the
    /// entire point of an unarchive. Returns whether a runtime was installed; a failure
    /// is logged rather than propagated, because the archive record is already gone and
    /// surfacing the agent without a shell still beats losing it.
    fn spawn_plain_runtime_for_unarchived_pane(
        &mut self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
        terminal_id: &crate::terminal::TerminalId,
        rows: u16,
        cols: u16,
    ) -> bool {
        if self.terminal_runtimes.get(terminal_id).is_some() {
            return true;
        }
        let cwd = match self.state.terminals.get(terminal_id) {
            Some(terminal) => terminal.cwd.clone(),
            None => return false,
        };
        let Some(launch_env) = self.pane_launch_env(
            ws_idx,
            pane_id,
            crate::config::AccountLaunchEnv::unselected(),
        ) else {
            return false;
        };
        let runtime = match crate::terminal::TerminalRuntime::spawn(
            pane_id,
            rows,
            cols,
            cwd,
            self.state.pane_scrollback_limit_bytes,
            self.state.host_terminal_theme,
            self.state.host_terminal_appearance,
            crate::pane::PaneShellConfig::new(&self.state.default_shell, self.state.shell_mode),
            &launch_env,
            self.event_tx.clone(),
            self.render_notify.clone(),
            self.render_dirty.clone(),
        ) {
            Ok(runtime) => runtime,
            Err(err) => {
                tracing::warn!(
                    pane = pane_id.raw(),
                    terminal = %terminal_id,
                    err = %err,
                    "unarchive could not spawn a shell for the restored pane"
                );
                return false;
            }
        };
        self.terminal_runtimes.insert(terminal_id.clone(), runtime);
        true
    }

    pub(super) fn agent_archive_error_body(
        &self,
        err: AgentArchiveError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentArchiveError::Target(err) => self.agent_target_error_body(err),
            AgentArchiveError::NotAgent => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: "agent target does not currently host an agent".into(),
            },
            AgentArchiveError::Working => crate::api::schema::ErrorBody {
                code: "agent_working".into(),
                message: "agent is working / mid-turn; retry with force to archive anyway".into(),
            },
            AgentArchiveError::NoResumableSession => crate::api::schema::ErrorBody {
                code: "no_resumable_session".into(),
                message: "agent has no resumable session to preserve — not a herdr-launched agent, or none reported".into(),
            },
        }
    }

    pub(super) fn agent_unarchive_error_body(
        &self,
        err: AgentUnarchiveError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentUnarchiveError::NotFound => crate::api::schema::ErrorBody {
                code: "archived_agent_not_found".into(),
                message: "no archived agent matches that name or terminal id".into(),
            },
            AgentUnarchiveError::NoResumablePlan => crate::api::schema::ErrorBody {
                code: "no_resumable_session".into(),
                message: "archived agent has no resumable session for its harness".into(),
            },
            AgentUnarchiveError::SessionLost => crate::api::schema::ErrorBody {
                code: "session_lost".into(),
                message: "archived session file no longer exists; retry with --fresh to start a clean agent"
                    .into(),
            },
            AgentUnarchiveError::SessionInUse { pane } => crate::api::schema::ErrorBody {
                code: "session_in_use".into(),
                message: format!(
                    "pane {pane} is already running this session; retire it first, or retry with --fresh to start a clean agent"
                ),
            },
        }
    }

    pub(super) fn agent_forget_error_body(
        &self,
        err: AgentForgetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentForgetError::NotFound => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: "no archived agent matches that name or terminal id".into(),
            },
            AgentForgetError::Live => crate::api::schema::ErrorBody {
                code: "archived_agent_not_found".into(),
                message: "that agent is live, not archived; only archived agents can be forgotten — archive it first".into(),
            },
        }
    }

    pub(super) fn start_agent(
        &mut self,
        params: AgentStartParams,
    ) -> Result<(crate::api::schema::AgentInfo, Vec<String>), AgentStartError> {
        let name = params.name;
        if !valid_agent_name(&name) {
            return Err(AgentStartError::InvalidName);
        }
        let Some(kind) = crate::detect::parse_agent_label(&params.kind) else {
            return Err(AgentStartError::UnsupportedKind(params.kind));
        };
        if params
            .args
            .iter()
            .any(|arg| arg.chars().any(char::is_control))
        {
            return Err(AgentStartError::InvalidArgument);
        }
        let persisted_agent_session =
            crate::agent_resume::persisted_session_from_launch_args(kind, &params.args);
        let conflicts = self.agent_name_conflicts(&name, "");
        if !conflicts.is_empty() {
            return Err(AgentStartError::DuplicateName {
                name,
                candidates: conflicts,
            });
        }
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return Err(AgentStartError::TargetNotFound(params.pane_id));
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        let terminal = self
            .state
            .terminals
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetNotFound(params.pane_id.clone()))?;
        if terminal.is_agent_terminal() || terminal.managed_agent_kind().is_some() {
            return Err(AgentStartError::TargetBusy(params.pane_id));
        }
        let runtime = self
            .terminal_runtimes
            .get(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        let shell_name = available_shell_name(runtime)
            .ok_or_else(|| AgentStartError::TargetBusy(params.pane_id.clone()))?;

        // Resolve an optional credential/config-home account for this agent. The
        // pane's shell already exists, so the account env is prepended to the
        // typed command as an `env VAR=value ...` prefix (the value is a config
        // dir PATH, never a credential). No account keeps the argv byte-identical.
        let account_env = match params.account.as_deref() {
            Some(account_id) => {
                let env = self
                    .resolve_account_launch_env(account_id, crate::detect::agent_label(kind))
                    .map_err(AgentStartError::from_account_resolve)?;
                Some((account_id.to_string(), env))
            }
            None => None,
        };

        let argv = agent_launch_argv(
            account_env.as_ref().map(|(_, env)| env),
            crate::detect::interactive_agent_executable(kind),
            params.args,
        );
        let command = crate::platform::interactive_shell_command(&argv, &shell_name)
            .ok_or(AgentStartError::InvalidArgument)?;
        let bytes = crate::app::api_helpers::encode_api_submission(runtime, &command);
        let timeout = Duration::from_millis(
            params
                .timeout_ms
                .unwrap_or(DEFAULT_AGENT_START_TIMEOUT.as_millis() as u64),
        );
        if timeout <= AGENT_START_SETTLE_DELAY || timeout > MAX_AGENT_START_TIMEOUT {
            return Err(AgentStartError::InvalidTimeout);
        }

        runtime
            .validate_input_submission(&bytes)
            .map_err(|err| AgentStartError::InputFailed(err.to_string()))?;

        let now = Instant::now();
        let terminal = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| AgentStartError::TargetUnavailable(params.pane_id.clone()))?;
        terminal.begin_managed_agent(name.clone(), kind, now, AGENT_START_SETTLE_DELAY, timeout);
        if let Some((account_id, _)) = &account_env {
            terminal.agent_account = Some(account_id.clone());
        }
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            terminal.clear_agent_name();
            return Err(AgentStartError::InputFailed(err.to_string()));
        }
        if let Some(session) = persisted_agent_session {
            terminal.set_managed_agent_launch_session(session);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();

        let agent = self
            .agent_info(ws_idx, pane_id)
            .ok_or(AgentStartError::TargetUnavailable(params.pane_id))?;
        Ok((agent, argv))
    }

    pub(super) fn agent_start_error_body(
        &self,
        err: AgentStartError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentStartError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentStartError::UnsupportedKind(kind) => crate::api::schema::ErrorBody {
                code: "unsupported_agent_kind".into(),
                message: format!("unsupported interactive agent kind {kind}"),
            },
            AgentStartError::InvalidArgument => crate::api::schema::ErrorBody {
                code: "invalid_agent_argument".into(),
                message: "agent arguments cannot be encoded safely for the target shell".into(),
            },
            AgentStartError::InvalidTimeout => crate::api::schema::ErrorBody {
                code: "invalid_agent_timeout".into(),
                message: INVALID_AGENT_TIMEOUT_MESSAGE.into(),
            },
            AgentStartError::TargetNotFound(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_not_found".into(),
                message: format!("agent target pane {target} not found"),
            },
            AgentStartError::TargetBusy(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_busy".into(),
                message: format!("agent target pane {target} is not an available shell"),
            },
            AgentStartError::TargetUnavailable(target) => crate::api::schema::ErrorBody {
                code: "agent_pane_unavailable".into(),
                message: format!("agent target pane {target} has no live terminal"),
            },
            AgentStartError::InputFailed(message) => crate::api::schema::ErrorBody {
                code: "agent_start_input_failed".into(),
                message,
            },
            AgentStartError::UnknownAccount(account) => crate::api::schema::ErrorBody {
                code: "unknown_account".into(),
                message: format!(
                    "no configured account with id {account} for this agent kind"
                ),
            },
            AgentStartError::AccountKindMismatch {
                account,
                account_kind,
                agent_kind,
            } => crate::api::schema::ErrorBody {
                code: "account_kind_mismatch".into(),
                message: format!(
                    "account {account} is for kind {account_kind}, not {agent_kind}"
                ),
            },
            AgentStartError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_target_error_body(
        &self,
        err: TerminalTargetError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            TerminalTargetError::NotFound { target } => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: format!("agent target {target} not found"),
            },
            TerminalTargetError::Ambiguous { target, candidates } => {
                crate::api::schema::ErrorBody {
                    code: "agent_target_ambiguous".into(),
                    message: format!(
                        "agent target {target} is ambiguous; candidates: {}",
                        candidates
                            .into_iter()
                            .map(|candidate| format!(
                                "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                                candidate.terminal_id,
                                candidate.pane_id,
                                candidate.workspace_id,
                                candidate.tab_id,
                                candidate.cwd.unwrap_or_else(|| "unknown".into()),
                                candidate.agent_status,
                            ))
                            .collect::<Vec<_>>()
                            .join("; ")
                    ),
                }
            }
        }
    }

    pub(super) fn agent_rename_error_body(
        &self,
        err: AgentRenameError,
    ) -> crate::api::schema::ErrorBody {
        match err {
            AgentRenameError::Target(err) => self.agent_target_error_body(err),
            AgentRenameError::InvalidName => crate::api::schema::ErrorBody {
                code: "invalid_agent_name".into(),
                message: INVALID_AGENT_NAME_MESSAGE.into(),
            },
            AgentRenameError::NotAgent => crate::api::schema::ErrorBody {
                code: "agent_not_found".into(),
                message: "agent target does not currently host an agent".into(),
            },
            AgentRenameError::PendingLaunch => crate::api::schema::ErrorBody {
                code: "agent_launch_pending".into(),
                message: "agent name cannot change while startup is pending".into(),
            },
            AgentRenameError::DuplicateName { name, candidates } => crate::api::schema::ErrorBody {
                code: "agent_name_taken".into(),
                message: format!(
                    "agent name {name} is already used; candidates: {}",
                    candidates
                        .into_iter()
                        .map(|candidate| format!(
                            "terminal_id={} pane_id={} workspace_id={} tab_id={} cwd={} status={:?}",
                            candidate.terminal_id,
                            candidate.pane_id,
                            candidate.workspace_id,
                            candidate.tab_id,
                            candidate.cwd.unwrap_or_else(|| "unknown".into()),
                            candidate.agent_status,
                        ))
                        .collect::<Vec<_>>()
                        .join("; ")
                ),
            },
        }
    }

    pub(super) fn agent_info(
        &self,
        ws_idx: usize,
        pane_id: crate::layout::PaneId,
    ) -> Option<crate::api::schema::AgentInfo> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let pane_state = ws.pane_state(pane_id)?;
        let terminal = self.state.terminals.get(&pane_state.attached_terminal_id)?;
        if !terminal.is_agent_terminal() {
            return None;
        }
        let pane = self.pane_metadata(ws_idx, pane_id)?;
        // Account routing, read from the RECORDED account and resolved against the live
        // registry. `account_config_dir` is None either because no account is recorded or
        // because the recorded one is gone; `account_unresolved` separates those.
        let account = terminal.agent_account.clone();
        let resolved_account = account.as_deref().and_then(|id| {
            self.loaded_accounts
                .iter()
                .find(|candidate| candidate.id == id)
        });
        let account_unresolved = account.is_some() && resolved_account.is_none();
        let account_config_dir = resolved_account.map(|entry| entry.config_dir.clone());
        Some(crate::api::schema::AgentInfo {
            terminal_id: pane.terminal_id,
            name: terminal.agent_name.clone(),
            agent: pane.agent,
            title: pane.title,
            terminal_title: pane.terminal_title,
            terminal_title_stripped: pane.terminal_title_stripped,
            display_agent: pane.display_agent,
            agent_status: pane.agent_status,
            input_pending: pane.input_pending,
            input_prompt_kind: pane.input_prompt_kind,
            composer: pane.composer,
            screen_detection_skipped: terminal.full_lifecycle_hook_authority_active(),
            state_labels: pane.state_labels,
            tokens: pane.tokens,
            agent_session: pane.agent_session,
            last_completed_turn: pane.last_completed_turn,
            turn: pane.turn,
            turn_epoch: pane.turn_epoch,
            workspace_id: pane.workspace_id,
            tab_id: pane.tab_id,
            pane_id: pane.pane_id,
            focused: pane.focused,
            launch_pending: terminal.managed_agent_launch_pending(),
            interactive_ready: terminal.managed_agent_interactive_ready(),
            state_change_seq: terminal.last_agent_state_change_seq.unwrap_or(0),
            status_since_unix_ms: terminal.agent_status_since_unix_ms,
            completion_seq: terminal.last_agent_completion_seq,
            cwd: pane.cwd,
            foreground_cwd: pane.foreground_cwd,
            revision: pane.revision,
            // Local agents carry no federation stamping; these serialize away.
            machine_id: None,
            machine_profile_id: None,
            machine_label: None,
            origin_machine_id: Some(crate::persist::machine::get_or_create()),
            reachability: None,
            last_known_status: None,
            // A live pane is never archived; the archived list is emitted
            // separately in `collect_agent_infos`.
            archived: None,
            parked_work: Vec::new(),
            account,
            account_config_dir,
            account_unresolved,
            session_transfer: terminal
                .session_transfer
                .as_ref()
                .map(|transfer| transfer.info()),
        })
    }

    fn agent_name_conflicts(
        &self,
        name: &str,
        except_terminal_id: &str,
    ) -> Vec<crate::api::schema::AgentInfo> {
        self.collect_agent_infos()
            .into_iter()
            .filter(|agent| {
                agent.name.as_deref() == Some(name) && agent.terminal_id != except_terminal_id
            })
            .collect()
    }
}

fn available_shell_name(runtime: &crate::terminal::TerminalRuntime) -> Option<String> {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return Some("sh".into());
    }
    crate::platform::available_pane_shell(runtime.child_pid()?)
}

/// Build the argv typed into an `agent.start` pane's existing shell. When an
/// account is selected, its config-home env is applied via an `env VAR=value …`
/// prefix that exec-replaces itself with the agent, so detection still sees the
/// harness process. Without an account the argv is `[executable, ..args]`.
fn agent_launch_argv(
    account_env: Option<&crate::config::AccountLaunchEnv>,
    executable: &str,
    args: Vec<String>,
) -> Vec<String> {
    let mut argv = Vec::new();
    // Only a launch with NOTHING to set and NOTHING to clear is byte-identical to no
    // account. A primary account on the harness default config-home sets no override
    // (issue #94) but still has a token to clear, and treating that as "nothing to do"
    // let a machine-global token silently outrank an explicit account selection.
    if let Some(env) = account_env.filter(|env| !env.is_empty()) {
        argv.push("env".to_string());
        // Clear conflicting auth tokens FIRST so the account selection is
        // authoritative. A machine-global CLAUDE_CODE_OAUTH_TOKEN otherwise wins over
        // CLAUDE_CONFIG_DIR and the swap silently keeps the old authenticated account
        // (gitmoot workflow-note row 86147).
        for var in &env.clear_vars {
            argv.push("-u".to_string());
            argv.push(var.clone());
        }
        for (key, value) in &env.vars {
            argv.push(format!("{key}={value}"));
        }
    }
    argv.push(executable.to_string());
    argv.extend(args);
    argv
}

pub(super) fn runtime_hosts_agent(
    runtime: &crate::terminal::TerminalRuntime,
    expected: crate::detect::Agent,
) -> bool {
    #[cfg(test)]
    if runtime.child_pid().is_none() {
        return true;
    }
    live_runtime_agent(runtime) == Some(expected)
}

fn live_runtime_agent(runtime: &crate::terminal::TerminalRuntime) -> Option<crate::detect::Agent> {
    live_agent_for_pid(runtime.child_pid()?)
}

fn live_agent_for_pid(pid: u32) -> Option<crate::detect::Agent> {
    live_agent_in_job(&crate::detect::foreground_job(pid)?)
}

fn live_agent_in_job(job: &crate::platform::ForegroundJob) -> Option<crate::detect::Agent> {
    crate::detect::identify_agent_in_job(job)
        .map(|(agent, _)| agent)
        .or_else(|| {
            job.processes
                .iter()
                .find_map(|process| crate::platform::process_agent_hint(process.pid))
        })
}

/// Decides whether the pane still hosts the same agent process.
///
/// Split out from the guard closure deliberately: the closure short-circuits on
/// `child_pid() == None`, which is always the case in the harness, so the real
/// comparison had no test at all — a reviewer removed the `same_instance`
/// conjunction and every test still passed.
///
/// Both conditions are required. The kind is identical across a restart by
/// definition, so kind alone lets a restarted agent through. The group alone
/// would accept a different agent adopted into the same group. Neither is
/// sufficient, and together they are still only a narrowing — a wrapper that
/// restarts the agent inside the existing group defeats both (see #26 review).
pub(super) fn occupant_unchanged(
    expected_group: Option<u32>,
    expected: crate::detect::Agent,
    job: Option<&crate::platform::ForegroundJob>,
) -> bool {
    // No readable foreground job means we cannot confirm occupancy, and an
    // unconfirmed occupant must not receive a submitting key.
    let Some(job) = job else { return false };
    expected_group == Some(job.process_group_id) && live_agent_in_job(job) == Some(expected)
}

/// Builds a guard that re-answers "is this still the SAME agent process?" at the
/// moment it is called.
///
/// Comparing agent *kind* is not enough. A pane whose agent exits and restarts
/// inside the delay window hosts a different process that identifies as the same
/// kind, so a kind-only check passes and the submitting key lands in a fresh
/// session that never received the prompt text. The foreground process group id
/// distinguishes instances: a restarted agent gets a new one.
pub(super) fn capture_occupant_group(runtime: &crate::terminal::TerminalRuntime) -> Option<u32> {
    crate::detect::foreground_job(runtime.child_pid()?).map(|job| job.process_group_id)
}

pub(super) fn runtime_agent_guard(
    runtime: &crate::terminal::TerminalRuntime,
    expected: crate::detect::Agent,
    expected_group: Option<u32>,
) -> Box<dyn Fn() -> bool + Send + Sync> {
    let pid = runtime.child_pid();
    Box::new(move || match pid {
        Some(pid) => occupant_unchanged(
            expected_group,
            expected,
            crate::detect::foreground_job(pid).as_ref(),
        ),
        // Only the test harness has a runtime without a child pid; a real pane
        // always has one, so its absence in production cannot confirm occupancy.
        None => cfg!(test),
    })
}

pub(super) enum AgentStartError {
    InvalidName,
    UnsupportedKind(String),
    InvalidArgument,
    InvalidTimeout,
    TargetNotFound(String),
    TargetBusy(String),
    TargetUnavailable(String),
    InputFailed(String),
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
    UnknownAccount(String),
    AccountKindMismatch {
        account: String,
        account_kind: String,
        agent_kind: String,
    },
}

impl AgentStartError {
    fn from_account_resolve(err: crate::app::api::accounts::AccountResolveError) -> Self {
        match err {
            crate::app::api::accounts::AccountResolveError::Unknown(account) => {
                AgentStartError::UnknownAccount(account)
            }
            crate::app::api::accounts::AccountResolveError::KindMismatch {
                account,
                account_kind,
                agent_kind,
            } => AgentStartError::AccountKindMismatch {
                account,
                account_kind,
                agent_kind,
            },
        }
    }
}

pub(super) enum AgentRenameError {
    Target(TerminalTargetError),
    InvalidName,
    NotAgent,
    PendingLaunch,
    DuplicateName {
        name: String,
        candidates: Vec<crate::api::schema::AgentInfo>,
    },
}

pub(super) enum AgentArchiveError {
    Target(TerminalTargetError),
    NotAgent,
    Working,
    NoResumableSession,
}

pub(super) enum AgentUnarchiveError {
    NotFound,
    NoResumablePlan,
    SessionLost,
    /// A LIVE agent already holds this session. Resuming would put two processes on
    /// one transcript; the pane holding it is named so the operator can act.
    SessionInUse {
        pane: String,
    },
}

pub(super) enum AgentForgetError {
    /// Nothing — archived or live — answers to the target.
    NotFound,
    /// The target names a live agent, which has no archive record to forget.
    Live,
}

/// True when an archived record is addressed by `target` — its agent name or its
/// stored terminal id.
fn archived_matches_target(record: &crate::persist::ArchivedAgentSnapshot, target: &str) -> bool {
    record.name.as_deref() == Some(target) || record.terminal_id == target
}

/// Render an archived record as an [`AgentInfo`] for `agent.list`/archive
/// responses. It has no live pane, so the pane/workspace/tab ids are empty and
/// the status is idle; the `archived` block is the load-bearing signal.
fn archived_agent_info(
    record: &crate::persist::ArchivedAgentSnapshot,
) -> crate::api::schema::AgentInfo {
    crate::api::schema::AgentInfo {
        terminal_id: record.terminal_id.clone(),
        name: record.name.clone(),
        agent: Some(record.kind.clone()),
        title: None,
        terminal_title: None,
        terminal_title_stripped: None,
        display_agent: Some(record.kind.clone()),
        agent_status: crate::api::schema::AgentStatus::Idle,
        input_pending: false,
        input_prompt_kind: None,
        composer: Default::default(),
        screen_detection_skipped: false,
        state_labels: Default::default(),
        tokens: Default::default(),
        agent_session: Some(crate::api::schema::AgentSessionInfo {
            source: record.agent_session.source.clone(),
            agent: record.agent_session.agent.clone(),
            kind: record.agent_session.kind,
            value: record.agent_session.value.clone(),
        }),
        last_completed_turn: None,
        turn: None,
        turn_epoch: None,
        workspace_id: String::new(),
        tab_id: String::new(),
        pane_id: String::new(),
        focused: false,
        launch_pending: false,
        interactive_ready: false,
        state_change_seq: 0,
        status_since_unix_ms: None,
        completion_seq: None,
        cwd: Some(record.cwd.display().to_string()),
        foreground_cwd: None,
        revision: 0,
        machine_id: None,
        machine_profile_id: None,
        machine_label: None,
        origin_machine_id: None,
        reachability: None,
        last_known_status: None,
        archived: Some(crate::api::schema::AgentArchivedInfo {
            at: record.archived.at.clone(),
            by: record.archived.by.clone(),
            reason: record.archived.reason.clone(),
        }),
        parked_work: record.parked_work.clone(),
        // An archived agent has no live terminal, so no routing has been applied yet.
        // It resolves its account when it is unarchived and resumed.
        account: None,
        account_config_dir: None,
        account_unresolved: false,
        session_transfer: None,
    }
}

#[cfg(test)]
mod tests {

    use crate::platform::{ForegroundJob, ForegroundProcess};

    fn job(group: u32, pid: u32, name: &str) -> ForegroundJob {
        ForegroundJob {
            process_group_id: group,
            processes: vec![ForegroundProcess {
                pid,
                name: name.to_string(),
                argv0: Some(name.to_string()),
                argv: Some(vec![name.to_string()]),
                cmdline: Some(name.to_string()),
            }],
        }
    }

    /// The #26 guard's actual decision, which previously had NO test: the guard
    /// closure short-circuits on child_pid() == None, always true in the
    /// harness, so a reviewer deleted the same_instance conjunction and every
    /// test still passed.
    #[test]
    fn occupant_unchanged_requires_both_group_and_kind() {
        let agent = crate::detect::Agent::Claude;
        let original = job(4242, 4242, "claude");

        // Same group, same kind — the only case that may receive the key.
        assert!(super::occupant_unchanged(
            Some(4242),
            agent,
            Some(&original)
        ));

        // Agent restarted into a NEW group: same kind, different instance.
        // A kind-only check passes this; the guard must not.
        assert!(
            !super::occupant_unchanged(Some(4242), agent, Some(&job(5555, 5555, "claude"))),
            "a restarted agent in a new process group must not receive the key"
        );

        // A different agent adopted into the SAME group. A group-only check
        // passes this; the guard must not.
        assert!(
            !super::occupant_unchanged(Some(4242), agent, Some(&job(4242, 4242, "bash"))),
            "a different occupant in the same group must not receive the key"
        );
    }

    /// An unreadable foreground job is not permission to write. This is the
    /// fallback-platform path, where refusing is the safe answer.
    #[test]
    fn occupant_unchanged_refuses_when_the_job_cannot_be_read() {
        assert!(!super::occupant_unchanged(
            Some(4242),
            crate::detect::Agent::Claude,
            None
        ));
    }

    /// Nothing captured at schedule time means nothing to compare against.
    #[test]
    fn occupant_unchanged_refuses_without_a_captured_group() {
        assert!(!super::occupant_unchanged(
            None,
            crate::detect::Agent::Claude,
            Some(&job(4242, 4242, "claude"))
        ));
    }
    use super::{agent_launch_argv, valid_agent_name};

    #[test]
    fn agent_launch_argv_prefixes_account_env() {
        let env = crate::config::AccountLaunchEnv {
            vars: vec![("CODEX_HOME".to_string(), "/home/x/.codex-work".to_string())],
            clear_vars: Vec::new(),
        };
        assert_eq!(
            agent_launch_argv(Some(&env), "codex", vec!["--yolo".to_string()]),
            vec![
                "env".to_string(),
                "CODEX_HOME=/home/x/.codex-work".to_string(),
                "codex".to_string(),
                "--yolo".to_string(),
            ]
        );
    }

    #[test]
    fn agent_launch_argv_clears_claude_oauth_token_before_config_dir() {
        // A claude account override must clear the machine-global CLAUDE_CODE_OAUTH_TOKEN
        // (via `env -u`) so the config-home selects the account, not the inherited token
        // (workflow-note row 86147). The `-u` must precede the KEY=VALUE assignment.
        let env = crate::config::AccountLaunchEnv {
            vars: vec![(
                "CLAUDE_CONFIG_DIR".to_string(),
                "/root/.claude-2".to_string(),
            )],
            clear_vars: vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
        };
        assert_eq!(
            agent_launch_argv(
                Some(&env),
                "claude",
                vec!["--resume".to_string(), "abc".to_string()]
            ),
            vec![
                "env".to_string(),
                "-u".to_string(),
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "CLAUDE_CONFIG_DIR=/root/.claude-2".to_string(),
                "claude".to_string(),
                "--resume".to_string(),
                "abc".to_string(),
            ]
        );
    }

    #[test]
    fn agent_launch_argv_does_not_clear_tokens_for_codex() {
        // codex has no token lever today, so its override must not gain a `-u` flag.
        let env = crate::config::AccountLaunchEnv {
            vars: vec![("CODEX_HOME".to_string(), "/home/x/.codex-work".to_string())],
            clear_vars: Vec::new(),
        };
        let argv = agent_launch_argv(Some(&env), "codex", vec![]);
        assert!(!argv.iter().any(|a| a == "-u"), "unexpected -u in {argv:?}");
    }

    #[test]
    fn agent_launch_argv_without_account_is_byte_identical() {
        assert_eq!(
            agent_launch_argv(None, "claude", vec!["--resume".to_string()]),
            vec!["claude".to_string(), "--resume".to_string()]
        );
    }

    /// A primary CLAUDE account on the default config-home injects no config-home
    /// override (issue #94) but MUST still clear the conflicting token.
    ///
    /// This test previously asserted the opposite — that such an account launches
    /// byte-identically to no account at all. That was the bug: `agent.start` on the
    /// primary account inherited a machine-global CLAUDE_CODE_OAUTH_TOKEN, which
    /// outranks config-home routing, so the agent authenticated as whichever account
    /// minted the token and wrote to ITS transcript. "Sets no override" and "has
    /// nothing to clear" are different facts and conflating them is what cost history.
    #[test]
    fn a_default_config_home_account_still_clears_a_conflicting_token() {
        let primary_claude = crate::config::AccountLaunchEnv {
            vars: Vec::new(),
            clear_vars: vec!["CLAUDE_CODE_OAUTH_TOKEN".to_string()],
        };
        assert_eq!(
            agent_launch_argv(
                Some(&primary_claude),
                "claude",
                vec!["--resume".to_string()]
            ),
            vec![
                "env".to_string(),
                "-u".to_string(),
                "CLAUDE_CODE_OAUTH_TOKEN".to_string(),
                "claude".to_string(),
                "--resume".to_string(),
            ],
            "a selected primary account must still strip a token that outranks it"
        );
        // No config-home override is injected, which is the half issue #94 cares about.
        assert!(
            !agent_launch_argv(Some(&primary_claude), "claude", vec![])
                .iter()
                .any(|arg| arg.starts_with("CLAUDE_CONFIG_DIR=")),
            "injecting the override on a default config-home strands ~/.claude.json"
        );
    }

    /// The genuinely byte-identical case: an account with nothing to set AND nothing to
    /// clear (a codex primary — codex has no token lever) launches exactly as no account.
    #[test]
    fn an_account_with_nothing_to_apply_is_byte_identical() {
        let primary_codex = crate::config::AccountLaunchEnv::default();
        assert_eq!(
            agent_launch_argv(Some(&primary_codex), "codex", vec!["--yolo".to_string()]),
            agent_launch_argv(None, "codex", vec!["--yolo".to_string()])
        );
    }

    #[test]
    fn agent_names_use_a_small_cli_safe_grammar() {
        for name in ["a", "reviewer-one", "reviewer_2", &"a".repeat(32)] {
            assert!(valid_agent_name(name), "expected {name:?} to be valid");
        }
        for name in [
            "",
            " reviewer",
            "reviewer ",
            "reviewer one",
            "Reviewer",
            "1reviewer",
            "reviewer.one",
            &"a".repeat(33),
        ] {
            assert!(!valid_agent_name(name), "expected {name:?} to be invalid");
        }
    }
}
