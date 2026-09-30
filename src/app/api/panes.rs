use bytes::Bytes;

use crate::api::schema::{
    EventData, EventEnvelope, EventKind, PaneClearAgentAuthorityParams, PaneCurrentParams,
    PaneDirection, PaneEdgesParams, PaneEdgesResult, PaneFocusDirectionParams,
    PaneFocusDirectionReason, PaneFocusDirectionResult, PaneInfo, PaneInputSetParams,
    PaneLayoutPane, PaneLayoutParams, PaneLayoutRect, PaneLayoutSnapshot, PaneLayoutSplit,
    PaneListParams, PaneMoveDestination, PaneMoveParams, PaneMoveReason, PaneMoveResult,
    PaneNeighborParams, PaneNeighborResult, PaneProcessInfo, PaneProcessInfoParams,
    PaneProcessInfoProcess, PaneReadParams, PaneReadResult, PaneReleaseAgentParams,
    PaneRenameParams, PaneReportAgentParams, PaneReportAgentSessionParams,
    PaneReportMetadataParams, PaneResizeParams, PaneResizeReason, PaneResizeResult,
    PaneSendInputParams, PaneSendKeysParams, PaneSendTextIfOutcome, PaneSendTextIfParams,
    PaneSendTextParams, PaneSnoozeParams, PaneSplitParams, PaneSwapParams, PaneSwapReason,
    PaneSwapResult, PaneTarget, PaneWorkContextSetParams, PaneZoomMode, PaneZoomParams,
    PaneZoomReason, PaneZoomResult, ResponseResult,
};
use crate::app::actions::{PaneZoomCommand, PaneZoomNoopReason};
use crate::app::App;
#[cfg(test)]
use crate::app::Mode;
use crate::layout::{find_in_direction, NavDirection, PaneId};
use crate::pane::ConditionalInputResult;

use super::super::api_helpers::{
    detect_state_from_api, encode_api_keys, normalize_metadata_source, normalize_metadata_tokens,
    normalize_metadata_ttl, normalize_reported_agent_label, MAX_METADATA_TOKEN_KEYS_PER_RESOURCE,
};
#[cfg(test)]
use super::super::api_helpers::{METADATA_SOURCE_MAX_CHARS, METADATA_TTL_MAX_MS};
use super::responses::{encode_error, encode_success};

#[derive(Debug)]
pub(crate) enum PaneSendError {
    NotFound,
    Failed(String),
}

enum AgentReportPaneResolution {
    Pane((usize, PaneId)),
    Missing,
    Ignore,
}

enum UniqueAgentSessionPane {
    None,
    One((usize, PaneId)),
    Several,
}

impl App {
    pub(crate) fn try_send_text_to_pane(
        &mut self,
        public_pane_id: &str,
        text: &str,
    ) -> Result<(), PaneSendError> {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(public_pane_id) else {
            return Err(PaneSendError::NotFound);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return Err(PaneSendError::NotFound);
        };
        if let Err(err) = runtime.try_send_bytes(Bytes::copy_from_slice(text.as_bytes())) {
            return Err(PaneSendError::Failed(err.to_string()));
        }
        if !text.is_empty() {
            self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
        }
        Ok(())
    }
}

impl App {
    pub(super) fn handle_pane_split(&mut self, id: String, params: PaneSplitParams) -> String {
        self.handle_pane_split_with_companion(id, params, false)
    }

    pub(super) fn handle_pane_split_companion(
        &mut self,
        id: String,
        params: PaneSplitParams,
    ) -> String {
        self.handle_pane_split_with_companion(id, params, true)
    }

    fn handle_pane_split_with_companion(
        &mut self,
        id: String,
        params: PaneSplitParams,
        companion: bool,
    ) -> String {
        let work_context = match self.prepare_spawn_work_context(params.work_context.clone()) {
            Ok(context) => context,
            Err(message) => return encode_error(id, "invalid_work_context", message),
        };
        let target = if let Some(target_pane_id) = params.target_pane_id.as_deref() {
            self.parse_pane_id(target_pane_id)
        } else if let Some(workspace_id) = params.workspace_id.as_deref() {
            self.parse_workspace_id(workspace_id).and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        } else {
            self.state.active.and_then(|ws_idx| {
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            })
        };
        let Some((ws_idx, target_pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let extra_env = match super::env::normalize_launch_env(params.env) {
            Ok(env) => env,
            Err((code, message)) => return encode_error(id, &code, message),
        };
        let (rows, cols) = self.state.estimate_pane_size();
        let split_cwd = params.cwd.map(std::path::PathBuf::from).or_else(|| {
            let follow_cwd = self.launch_cwd_for_pane_in_workspace(ws_idx, target_pane_id);
            Some(self.resolve_new_terminal_cwd(follow_cwd))
        });
        let default_shell = self.state.default_shell.clone();
        let scrollback_limit_bytes = self.state.pane_scrollback_limit_bytes;
        let host_terminal_theme = self.state.pane_terminal_theme();
        let host_terminal_appearance = Some(self.state.pane_terminal_appearance());
        let previous_focus = self.state.current_pane_focus_target();
        let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let before = matches!(
            params.direction,
            crate::api::schema::SplitDirection::Left | crate::api::schema::SplitDirection::Up
        );
        let direction = match params.direction {
            crate::api::schema::SplitDirection::Left
            | crate::api::schema::SplitDirection::Right => ratatui::layout::Direction::Horizontal,
            crate::api::schema::SplitDirection::Up | crate::api::schema::SplitDirection::Down => {
                ratatui::layout::Direction::Vertical
            }
        };
        let shell_config = crate::pane::PaneShellConfig::new(&default_shell, self.state.shell_mode);
        let split_result = match params.ratio {
            Some(ratio) => ws.split_pane_with_ratio_and_placement(
                target_pane_id,
                direction,
                ratio,
                before,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                params.focus,
            ),
            None => ws.split_pane_with_placement(
                target_pane_id,
                direction,
                before,
                rows,
                cols,
                split_cwd,
                scrollback_limit_bytes,
                host_terminal_theme,
                host_terminal_appearance,
                shell_config,
                extra_env,
                params.focus,
            ),
        };
        let (target_tab_idx, mut new_pane) = match split_result {
            Some(Ok(result)) => result,
            Some(Err(err)) => return encode_error(id, "pane_split_failed", err.to_string()),
            None => return encode_error(id, "pane_not_found", "pane not found"),
        };
        if let Some(pane) = self.state.workspaces[ws_idx].pane_state_mut(new_pane.pane_id) {
            pane.is_companion = companion;
            pane.right_click_passthrough = matches!(
                params.right_click,
                crate::api::schema::PaneRightClickTarget::Pane
            );
        }
        Self::bind_spawn_work_context(&mut new_pane.terminal, work_context);
        if params.focus {
            self.state.switch_workspace_tab(ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, ws_idx, new_pane.pane_id);
            self.focus_client_on_pane();
        }
        self.terminal_runtimes
            .insert(new_pane.terminal.id.clone(), new_pane.runtime);
        self.state
            .remove_alias_shadowed_by_new_pane(new_pane.pane_id);
        self.state
            .terminals
            .insert(new_pane.terminal.id.clone(), new_pane.terminal);
        self.schedule_session_save();
        let pane = self.pane_info(ws_idx, new_pane.pane_id).unwrap();
        self.emit_event(EventEnvelope {
            event: EventKind::PaneCreated,
            data: EventData::PaneCreated { pane: pane.clone() },
        });
        self.emit_layout_updated_event(ws_idx, target_tab_idx);

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_list(&mut self, id: String, params: PaneListParams) -> String {
        match self.collect_panes_for_workspace(params.workspace_id.as_deref()) {
            Ok(panes) => encode_success(id, ResponseResult::PaneList { panes }),
            Err((code, message)) => encode_error(id, &code, message),
        }
    }

    pub(super) fn handle_pane_current(&mut self, id: String, params: PaneCurrentParams) -> String {
        let target = match params.caller_pane_id.as_deref() {
            Some(caller_pane_id) => self.parse_pane_id(caller_pane_id),
            None => self.resolve_optional_pane(None),
        };
        let Some((ws_idx, pane_id)) = target else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        encode_success(id, ResponseResult::PaneCurrent { pane })
    }

    pub(super) fn handle_pane_get(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_settlement(
        &mut self,
        id: String,
        target: PaneTarget,
        settle: bool,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        if settle
            && self.state.workspaces[ws_idx]
                .pane_state(pane_id)
                .is_some_and(|pane| pane.snoozed_until().is_some())
        {
            return encode_error(id, "pane_snoozed", "unsnooze this pane before settling it");
        }
        if settle {
            self.state.settle_pane_at(
                ws_idx,
                pane_id,
                crate::app::settled::unix_seconds(std::time::SystemTime::now()),
            );
        } else {
            self.state
                .note_human_pane_activity_at(pane_id, std::time::Instant::now());
        }
        self.flush_pane_settlement_events();
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_snooze(&mut self, id: String, params: PaneSnoozeParams) -> String {
        let now = crate::app::settled::unix_seconds(std::time::SystemTime::now());
        self.handle_pane_snooze_at(id, params, now)
    }

    fn handle_pane_snooze_at(&mut self, id: String, params: PaneSnoozeParams, now: u64) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let deadline = match (params.duration_s, params.snoozed_until) {
            (Some(duration), None)
                if (1..=crate::app::settled::MAX_SNOOZE_SECONDS).contains(&duration) =>
            {
                let Some(deadline) = now.checked_add(duration) else {
                    return encode_error(
                        id,
                        "invalid_snooze_duration",
                        "duration must be between one second and seven days",
                    );
                };
                deadline
            }
            (Some(_), None) => {
                return encode_error(
                    id,
                    "invalid_snooze_duration",
                    "duration must be between one second and seven days",
                )
            }
            (None, Some(deadline))
                if deadline > now
                    && deadline.saturating_sub(now) <= crate::app::settled::MAX_SNOOZE_SECONDS =>
            {
                deadline
            }
            (None, Some(_)) => {
                return encode_error(
                    id,
                    "invalid_snooze_deadline",
                    "deadline must be in the future and within seven days",
                )
            }
            (None, None) | (Some(_), Some(_)) => {
                return encode_error(
                    id,
                    "invalid_snooze_request",
                    "provide exactly one of duration_s and snoozed_until",
                )
            }
        };
        if self.state.pane_is_settled(ws_idx, pane_id) {
            return encode_error(id, "pane_settled", "unsettle this pane before snoozing it");
        }
        let gated = self.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .and_then(|pane| {
                self.state
                    .terminals
                    .get(&pane.attached_terminal_id)
                    .map(|terminal| pane.agent_projection(terminal).needs_human_attention())
            })
            .unwrap_or(false);
        if gated {
            return encode_error(
                id,
                "pane_needs_attention",
                "resolve the active human-attention gate before snoozing",
            );
        }
        self.state.snooze_pane_at(ws_idx, pane_id, deadline);
        self.flush_pane_snooze_events();
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_unsnooze(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        self.state.unsnooze_pane_at(
            ws_idx,
            pane_id,
            crate::api::schema::PaneUnsnoozeReason::Explicit,
            std::time::Instant::now(),
        );
        self.flush_pane_snooze_events();
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_focus(&mut self, id: String, target: PaneTarget) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        let Some(_tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };

        self.state.focus_pane_in_workspace(ws_idx, pane_id);
        self.state.mark_active_pane_seen();
        self.focus_client_on_pane();

        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &target.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_layout(&mut self, id: String, params: PaneLayoutParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(id, ResponseResult::PaneLayout { layout })
    }

    pub(super) fn handle_pane_process_info(
        &mut self,
        id: String,
        params: PaneProcessInfoParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some((runtime, _workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let shell_pid = runtime.child_pid();
        let tty = runtime.tty_name().map(|path| path.display().to_string());
        let foreground_job = shell_pid.and_then(crate::detect::foreground_job);
        let foreground_process_group_id = foreground_job.as_ref().map(|job| job.process_group_id);
        let foreground_processes = foreground_job
            .map(|job| {
                job.processes
                    .into_iter()
                    .map(|process| PaneProcessInfoProcess {
                        pid: process.pid,
                        name: process.name,
                        argv0: process.argv0,
                        argv: process.argv,
                        cmdline: process.cmdline,
                        cwd: crate::platform::process_cwd(process.pid)
                            .map(|cwd| cwd.display().to_string()),
                    })
                    .collect()
            })
            .unwrap_or_default();

        encode_success(
            id,
            ResponseResult::PaneProcessInfo {
                process_info: PaneProcessInfo {
                    pane_id: public_pane_id,
                    shell_pid,
                    foreground_process_group_id,
                    tty,
                    foreground_processes,
                },
            },
        )
    }

    pub(super) fn handle_pane_neighbor(
        &mut self,
        id: String,
        params: PaneNeighborParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let neighbor_pane_id = self
            .directional_pane_target(ws_idx, tab_idx, pane_id, params.direction)
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneNeighbor {
                neighbor: PaneNeighborResult {
                    pane_id: source_public_id,
                    direction: params.direction,
                    neighbor_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_edges(&mut self, id: String, params: PaneEdgesParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(tab) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
        else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let area = self.state.view.terminal_area;
        let Some(info) = tab
            .layout
            .panes(area)
            .into_iter()
            .find(|info| info.id == pane_id)
        else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneEdges {
                edges: PaneEdgesResult {
                    pane_id: pane_public_id,
                    left: info.rect.x <= area.x,
                    right: info.rect.x + info.rect.width >= area.x + area.width,
                    up: info.rect.y <= area.y,
                    down: info.rect.y + info.rect.height >= area.y + area.height,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_focus_direction(
        &mut self,
        id: String,
        params: PaneFocusDirectionParams,
    ) -> String {
        let Some((ws_idx, source_pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref())
        else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return pane_not_found(
                id,
                &self
                    .public_pane_id(ws_idx, source_pane_id)
                    .unwrap_or_default(),
            );
        };
        let Some(source_public_id) = self.public_pane_id(ws_idx, source_pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let target =
            self.directional_pane_target(ws_idx, tab_idx, source_pane_id, params.direction);
        let reason = target
            .is_none()
            .then_some(PaneFocusDirectionReason::NoNeighbor);

        if let Some(target_pane_id) = target {
            self.state.focus_pane_in_workspace(ws_idx, target_pane_id);
            self.state.switch_workspace_tab(ws_idx, tab_idx);
            self.focus_client_on_pane();
        }
        let focused_pane_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.tabs.get(tab_idx))
            .map(|tab| tab.layout.focused())
            .and_then(|pane_id| self.public_pane_id(ws_idx, pane_id));
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };

        encode_success(
            id,
            ResponseResult::PaneFocusDirection {
                focus: PaneFocusDirectionResult {
                    changed: target.is_some(),
                    reason,
                    source_pane_id: source_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_resize(&mut self, id: String, params: PaneResizeParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };

        let amount = params
            .amount
            .filter(|amount| amount.is_finite())
            .unwrap_or(0.05)
            .abs()
            .min(0.5);
        let direction: NavDirection = params.direction.into();
        let area = self.state.view.terminal_area;
        let changed = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|ws| ws.tabs.get_mut(tab_idx))
            .is_some_and(|tab| tab.layout.resize_pane(pane_id, direction, amount, area));
        if changed {
            self.schedule_session_save();
        }

        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneResize {
                resize: PaneResizeResult {
                    changed,
                    reason: (!changed).then_some(PaneResizeReason::Unchanged),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_swap(&mut self, id: String, params: PaneSwapParams) -> String {
        let directional = params.direction.is_some();
        let explicit = params.source_pane_id.is_some() || params.target_pane_id.is_some();
        if directional == explicit {
            return encode_error(
                id,
                "invalid_pane_swap",
                "provide either direction with optional pane_id, or source_pane_id and target_pane_id",
            );
        }

        let (ws_idx, tab_idx, source_pane_id, target_pane_id, reason) = if let Some(direction) =
            params.direction
        {
            let Some((ws_idx, source_pane_id)) =
                self.resolve_swap_source(params.pane_id.as_deref())
            else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            let Some(tab_idx) =
                self.state.workspaces[ws_idx].find_tab_index_for_pane(source_pane_id)
            else {
                return pane_not_found(
                    id,
                    &self
                        .public_pane_id(ws_idx, source_pane_id)
                        .unwrap_or_default(),
                );
            };
            let target = self.directional_pane_target(ws_idx, tab_idx, source_pane_id, direction);
            match target {
                Some(target_pane_id) => {
                    (ws_idx, tab_idx, source_pane_id, Some(target_pane_id), None)
                }
                None => (
                    ws_idx,
                    tab_idx,
                    source_pane_id,
                    None,
                    Some(PaneSwapReason::NoNeighbor),
                ),
            }
        } else {
            let Some(source_raw) = params.source_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing source_pane_id");
            };
            let Some(target_raw) = params.target_pane_id.as_deref() else {
                return encode_error(id, "invalid_pane_swap", "missing target_pane_id");
            };
            let source = self
                .parse_pane_id(source_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let target = self
                .parse_pane_id(target_raw)
                .and_then(|(ws_idx, pane_id)| {
                    let tab_idx = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id)?;
                    Some((ws_idx, tab_idx, pane_id))
                });
            let response_context = source
                .map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx))
                .or_else(|| target.map(|(ws_idx, tab_idx, _)| (ws_idx, tab_idx)))
                .or_else(|| {
                    let ws_idx = self.state.active?;
                    let tab_idx = self.state.workspaces.get(ws_idx)?.active_tab_index();
                    Some((ws_idx, tab_idx))
                });
            let Some((ws_idx, tab_idx)) = response_context else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let source_pane_id = source
                .map(|(_, _, pane_id)| pane_id)
                .or_else(|| {
                    self.state
                        .workspaces
                        .get(ws_idx)?
                        .tabs
                        .get(tab_idx)
                        .map(|tab| tab.layout.focused())
                })
                .unwrap_or(PaneId::from_raw(0));
            let target_pane_id = target.map(|(_, _, pane_id)| pane_id);
            let reason = match (source, target) {
                (None, _) | (_, None) => Some(PaneSwapReason::NotFound),
                (Some((_, _, source)), Some((_, _, target))) if source == target => {
                    Some(PaneSwapReason::SamePane)
                }
                (Some((source_ws, source_tab, _)), Some((target_ws, target_tab, _)))
                    if source_ws != target_ws || source_tab != target_tab =>
                {
                    Some(PaneSwapReason::CrossTab)
                }
                _ => None,
            };
            (ws_idx, tab_idx, source_pane_id, target_pane_id, reason)
        };

        let mut changed = false;
        if reason.is_none() {
            if let Some(target_pane_id) = target_pane_id {
                let previous_focus = self.state.current_pane_focus_target();
                if let Some(tab) = self
                    .state
                    .workspaces
                    .get_mut(ws_idx)
                    .and_then(|ws| ws.tabs.get_mut(tab_idx))
                {
                    changed = tab.layout.swap_panes(source_pane_id, target_pane_id);
                    tab.layout.focus_pane(source_pane_id);
                    if changed {
                        self.state.switch_workspace_tab(ws_idx, tab_idx);
                        self.state
                            .record_pane_focus_change(previous_focus, ws_idx, source_pane_id);
                        self.state.mark_session_dirty();
                        self.schedule_session_save();
                    }
                }
            }
        }

        let source_public_id = match params.source_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .unwrap_or(raw),
            None => self
                .public_pane_id(ws_idx, source_pane_id)
                .unwrap_or_default(),
        };
        let target_public_id = match params.target_pane_id {
            Some(raw) => self
                .parse_pane_id(&raw)
                .and_then(|(idx, pane_id)| {
                    self.state
                        .workspaces
                        .get(idx)?
                        .find_tab_index_for_pane(pane_id)?;
                    self.public_pane_id(idx, pane_id)
                })
                .or(Some(raw)),
            None => target_pane_id.and_then(|pane_id| self.public_pane_id(ws_idx, pane_id)),
        };
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneSwap {
                swap: PaneSwapResult {
                    changed,
                    reason,
                    source_pane_id: source_public_id,
                    target_pane_id: target_public_id,
                    focused_pane_id,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_move(&mut self, id: String, params: PaneMoveParams) -> String {
        let PaneMoveParams {
            pane_id,
            destination,
            focus,
        } = params;
        let Some((source_ws_idx, source_pane_id)) = self.parse_pane_id(&pane_id) else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let Some(source_tab_idx) =
            self.state.workspaces[source_ws_idx].find_tab_index_for_pane(source_pane_id)
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let previous_pane_id = self
            .public_pane_id(source_ws_idx, source_pane_id)
            .unwrap_or_else(|| pane_id.clone());
        let previous_workspace_id = self.public_workspace_id(source_ws_idx);
        let Some(previous_tab_id) = self.public_tab_id(source_ws_idx, source_tab_idx) else {
            return encode_error(id, "tab_not_found", "source tab not found");
        };
        let Some(source_terminal_id) = self
            .state
            .workspaces
            .get(source_ws_idx)
            .and_then(|ws| ws.tabs.get(source_tab_idx))
            .and_then(|tab| tab.terminal_id(source_pane_id))
            .cloned()
        else {
            return encode_error(id, "pane_not_found", "source pane not found");
        };
        let recovery_context = PaneMoveRecoveryContext {
            source_ws_idx,
            previous_workspace_id: previous_workspace_id.clone(),
            previous_workspace_label: self.state.workspaces[source_ws_idx].custom_name.clone(),
            previous_tab_label: self.state.workspaces[source_ws_idx].tabs[source_tab_idx]
                .custom_name
                .clone(),
            previous_worktree_space: self.state.workspaces[source_ws_idx].worktree_space.clone(),
            previous_repo_binding: self.state.workspaces[source_ws_idx].repo_binding.clone(),
            previous_repo_binding_cleared: self.state.workspaces[source_ws_idx]
                .repo_binding_cleared,
            identity_cwd: self.state.workspaces[source_ws_idx].identity_cwd.clone(),
        };

        if self.state.workspaces[source_ws_idx].tabs[source_tab_idx].zoomed {
            let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx) else {
                return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
            };
            let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                return encode_error(id, "pane_not_found", "source pane not found");
            };
            return encode_unchanged_pane_move(
                id,
                PaneMoveReason::ZoomedTab,
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane,
                Some(layout.clone()),
                layout,
            );
        }

        let resolved = match destination {
            PaneMoveDestination::Tab {
                tab_id,
                target_pane_id,
                split,
                ratio,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                if source_ws_idx == target_ws_idx && source_tab_idx == target_tab_idx {
                    let Some(layout) = self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::SameTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(layout.clone()),
                        layout,
                    );
                }
                if self.state.workspaces[target_ws_idx].tabs[target_tab_idx].zoomed {
                    let Some(source_layout) =
                        self.pane_layout_snapshot(source_ws_idx, source_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(target_layout) =
                        self.pane_layout_snapshot(target_ws_idx, target_tab_idx)
                    else {
                        return encode_error(
                            id,
                            "pane_layout_unavailable",
                            "pane layout unavailable",
                        );
                    };
                    let Some(pane) = self.pane_info(source_ws_idx, source_pane_id) else {
                        return encode_error(id, "pane_not_found", "source pane not found");
                    };
                    return encode_unchanged_pane_move(
                        id,
                        PaneMoveReason::ZoomedTab,
                        previous_pane_id,
                        previous_workspace_id,
                        previous_tab_id,
                        pane,
                        Some(source_layout),
                        target_layout,
                    );
                }
                let target_pane_id = match target_pane_id {
                    Some(raw) => {
                        let Some((pane_ws_idx, pane_id)) = self.parse_pane_id(&raw) else {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} not found"),
                            );
                        };
                        let pane_tab_idx =
                            self.state.workspaces[pane_ws_idx].find_tab_index_for_pane(pane_id);
                        if pane_ws_idx != target_ws_idx || pane_tab_idx != Some(target_tab_idx) {
                            return encode_error(
                                id,
                                "target_pane_not_found",
                                format!("target pane {raw} is not in tab {tab_id}"),
                            );
                        }
                        pane_id
                    }
                    None => self.state.workspaces[target_ws_idx].tabs[target_tab_idx]
                        .layout
                        .focused(),
                };
                let Some(target_tab_id) = self.public_tab_id(target_ws_idx, target_tab_idx) else {
                    return encode_error(id, "tab_not_found", format!("tab {tab_id} not found"));
                };
                ResolvedPaneMoveDestination::ExistingTab {
                    tab_id: target_tab_id,
                    target_pane_id,
                    split,
                    ratio: ratio.unwrap_or(0.5),
                    cross_workspace: source_ws_idx != target_ws_idx,
                }
            }
            PaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let target_workspace_id = if let Some(workspace_id) = workspace_id {
                    let Some(ws_idx) = self.parse_workspace_id(&workspace_id) else {
                        return encode_error(
                            id,
                            "workspace_not_found",
                            format!("workspace {workspace_id} not found"),
                        );
                    };
                    self.public_workspace_id(ws_idx)
                } else {
                    previous_workspace_id.clone()
                };
                ResolvedPaneMoveDestination::NewTab {
                    workspace_id: target_workspace_id,
                    label,
                }
            }
            PaneMoveDestination::NewWorkspace { label, tab_label } => {
                ResolvedPaneMoveDestination::NewWorkspace { label, tab_label }
            }
        };

        let previous_focus = self.state.current_pane_focus_target();
        let taken = match self
            .state
            .workspaces
            .get_mut(source_ws_idx)
            .and_then(|ws| ws.take_pane_for_move(source_pane_id))
        {
            Some(taken) => taken,
            None => return encode_error(id, "pane_move_failed", "source pane could not be moved"),
        };
        let source_removed_tab_id = taken.removed_tab_idx.map(|_| previous_tab_id.clone());
        let source_workspace_empty = taken.workspace_empty;
        let moved = taken.moved;
        let cross_workspace = match &resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                cross_workspace, ..
            } => *cross_workspace,
            ResolvedPaneMoveDestination::NewTab { workspace_id, .. } => {
                workspace_id != &previous_workspace_id
            }
            ResolvedPaneMoveDestination::NewWorkspace { .. } => true,
        };
        if cross_workspace {
            if let Some(ws) = self.state.workspaces.get_mut(source_ws_idx) {
                ws.unregister_moved_pane(source_pane_id);
            }
            self.state
                .public_pane_id_aliases
                .insert(previous_pane_id.clone(), source_pane_id);
        }

        let mut closed_workspace_id = None;
        if source_workspace_empty && cross_workspace {
            self.state.workspaces.remove(source_ws_idx);
            closed_workspace_id = Some(previous_workspace_id.clone());
            if self.state.workspaces.is_empty() {
                self.state.active = None;
                self.state.selected = 0;
            } else {
                if let Some(active) = self.state.active {
                    if active == source_ws_idx {
                        self.state.active =
                            Some(source_ws_idx.min(self.state.workspaces.len() - 1));
                    } else if active > source_ws_idx {
                        self.state.active = Some(active - 1);
                    }
                }
                if self.state.selected == source_ws_idx {
                    self.state.selected = source_ws_idx.min(self.state.workspaces.len() - 1);
                } else if self.state.selected > source_ws_idx {
                    self.state.selected -= 1;
                }
            }
        }

        let mut created_workspace = false;
        let mut created_tab = false;
        let (target_ws_idx, target_tab_idx, moved_pane_id) = match resolved {
            ResolvedPaneMoveDestination::ExistingTab {
                tab_id,
                target_pane_id,
                split,
                ratio,
                cross_workspace: _,
            } => {
                let Some((target_ws_idx, target_tab_idx)) = self.parse_tab_id(&tab_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target tab disappeared");
                };
                let (direction, before) = split_direction_to_layout(split);
                let moved_pane_id = match self.state.workspaces[target_ws_idx]
                    .insert_moved_pane_into_tab(
                        target_tab_idx,
                        target_pane_id,
                        moved,
                        direction,
                        ratio,
                        before,
                        focus,
                    ) {
                    Ok(pane_id) => pane_id,
                    Err(moved) => {
                        self.recover_failed_pane_move(recovery_context, *moved);
                        return encode_error(
                            id,
                            "pane_move_failed",
                            "target pane could not be split",
                        );
                    }
                };
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewTab {
                workspace_id,
                label,
            } => {
                let Some(target_ws_idx) = self.parse_workspace_id(&workspace_id) else {
                    self.recover_failed_pane_move(recovery_context, moved);
                    return encode_error(id, "pane_move_failed", "target workspace disappeared");
                };
                let moved_pane_id = moved.pane_id;
                let target_tab_idx = self.state.workspaces[target_ws_idx]
                    .create_tab_from_existing_pane(
                        moved,
                        label,
                        self.event_tx.clone(),
                        self.render_notify.clone(),
                        self.render_dirty.clone(),
                    );
                created_tab = true;
                (target_ws_idx, target_tab_idx, moved_pane_id)
            }
            ResolvedPaneMoveDestination::NewWorkspace { label, tab_label } => {
                let identity_cwd = self
                    .state
                    .terminals
                    .get(&source_terminal_id)
                    .map(|terminal| terminal.cwd.clone())
                    .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
                let moved_pane_id = moved.pane_id;
                let workspace = crate::workspace::Workspace::from_existing_pane(
                    label,
                    tab_label,
                    identity_cwd,
                    moved,
                    self.event_tx.clone(),
                    self.render_notify.clone(),
                    self.render_dirty.clone(),
                );
                self.state.workspaces.push(workspace);
                let target_ws_idx = self.state.workspaces.len() - 1;
                created_workspace = true;
                created_tab = true;
                (target_ws_idx, 0, moved_pane_id)
            }
        };

        if focus || self.state.active.is_none() {
            self.state
                .switch_workspace_tab(target_ws_idx, target_tab_idx);
            self.state
                .record_pane_focus_change(previous_focus, target_ws_idx, moved_pane_id);
            self.focus_client_on_pane();
        }
        let created_workspace = created_workspace.then(|| self.workspace_info(target_ws_idx));
        let created_tab = if created_tab {
            self.tab_info(target_ws_idx, target_tab_idx)
        } else {
            None
        };

        self.state.remove_alias_shadowed_by_new_pane(moved_pane_id);
        self.state.mark_session_dirty();
        self.schedule_session_save();
        let Some(pane) = self.pane_info(target_ws_idx, moved_pane_id) else {
            return encode_error(id, "pane_move_failed", "moved pane is unavailable");
        };
        let source_layout = if closed_workspace_id.is_none() {
            self.parse_tab_id(&previous_tab_id)
                .and_then(|(ws_idx, tab_idx)| self.pane_layout_snapshot(ws_idx, tab_idx))
        } else {
            None
        };
        let Some(target_layout) = self.pane_layout_snapshot(target_ws_idx, target_tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = target_layout.focused_pane_id.clone();
        let move_result = PaneMoveResult {
            changed: true,
            reason: None,
            previous_pane_id: previous_pane_id.clone(),
            previous_workspace_id: previous_workspace_id.clone(),
            previous_tab_id: previous_tab_id.clone(),
            pane: Box::new(pane.clone()),
            source_layout: source_layout.clone().map(Box::new),
            target_layout: Box::new(target_layout),
            created_workspace: created_workspace.clone(),
            created_tab: created_tab.clone(),
            closed_workspace_id: closed_workspace_id.clone(),
            closed_tab_id: source_removed_tab_id.clone(),
            focused_pane_id,
        };
        if let Some(closed_tab_id) = &source_removed_tab_id {
            self.emit_event(EventEnvelope {
                event: EventKind::TabClosed,
                data: EventData::TabClosed {
                    tab_id: closed_tab_id.clone(),
                    workspace_id: previous_workspace_id.clone(),
                },
            });
        }
        if let Some(closed_workspace_id) = &closed_workspace_id {
            crate::logging::workspace_closed(closed_workspace_id);
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id: closed_workspace_id.clone(),
                    workspace: None,
                },
            });
        }
        if let Some(workspace) = &created_workspace {
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceCreated,
                data: EventData::WorkspaceCreated {
                    workspace: workspace.clone(),
                },
            });
        }
        if let Some(tab) = &created_tab {
            self.emit_event(EventEnvelope {
                event: EventKind::TabCreated,
                data: EventData::TabCreated { tab: tab.clone() },
            });
        }
        self.emit_event(EventEnvelope {
            event: EventKind::PaneMoved,
            data: EventData::PaneMoved {
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                created_workspace,
                created_tab,
                closed_workspace_id,
                closed_tab_id: source_removed_tab_id,
            },
        });
        if let Some(source_layout) = source_layout {
            self.emit_layout_updated_snapshot(source_layout);
        }
        self.emit_layout_updated_snapshot((*move_result.target_layout).clone());

        encode_success(id, ResponseResult::PaneMove { move_result })
    }

    fn recover_failed_pane_move(
        &mut self,
        context: PaneMoveRecoveryContext,
        moved: crate::workspace::MovedPane,
    ) {
        if let Some(ws_idx) = self.parse_workspace_id(&context.previous_workspace_id) {
            self.state.workspaces[ws_idx].create_tab_from_existing_pane(
                moved,
                context.previous_tab_label,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
        } else {
            let mut workspace = crate::workspace::Workspace::from_existing_pane(
                context.previous_workspace_label,
                context.previous_tab_label,
                context.identity_cwd,
                moved,
                self.event_tx.clone(),
                self.render_notify.clone(),
                self.render_dirty.clone(),
            );
            workspace.id = context.previous_workspace_id;
            workspace.worktree_space = context.previous_worktree_space;
            workspace.repo_binding = context.previous_repo_binding;
            workspace.repo_binding_cleared = context.previous_repo_binding_cleared;
            let insert_idx = context.source_ws_idx.min(self.state.workspaces.len());
            if let Some(active) = self.state.active {
                if active >= insert_idx {
                    self.state.active = Some(active + 1);
                }
            }
            if self.state.selected >= insert_idx && !self.state.workspaces.is_empty() {
                self.state.selected += 1;
            }
            self.state.workspaces.insert(insert_idx, workspace);
        }
        self.state.mark_session_dirty();
        self.schedule_session_save();
    }

    pub(super) fn handle_pane_zoom(&mut self, id: String, params: PaneZoomParams) -> String {
        let Some((ws_idx, pane_id)) = self.resolve_optional_pane(params.pane_id.as_deref()) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let Some(tab_idx) = self.state.workspaces[ws_idx].find_tab_index_for_pane(pane_id) else {
            return pane_not_found(
                id,
                &self.public_pane_id(ws_idx, pane_id).unwrap_or_default(),
            );
        };
        let Some(pane_public_id) = self.public_pane_id(ws_idx, pane_id) else {
            return encode_error(id, "pane_not_found", "pane not found");
        };
        let command = match params.mode {
            PaneZoomMode::Toggle => PaneZoomCommand::Toggle,
            PaneZoomMode::On => PaneZoomCommand::On,
            PaneZoomMode::Off => PaneZoomCommand::Off,
        };
        let Some(outcome) = self.state.apply_pane_zoom(ws_idx, pane_id, command) else {
            return pane_not_found(id, &pane_public_id);
        };
        if outcome.changed || outcome.focus_changed {
            self.schedule_session_save();
        }
        self.focus_client_on_pane();
        let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) else {
            return encode_error(id, "pane_layout_unavailable", "pane layout unavailable");
        };
        let focused_pane_id = layout.focused_pane_id.clone();
        if outcome.changed || outcome.focus_changed {
            self.emit_layout_updated_snapshot(layout.clone());
        }

        encode_success(
            id,
            ResponseResult::PaneZoom {
                zoom: PaneZoomResult {
                    changed: outcome.changed || outcome.focus_changed,
                    zoom_changed: outcome.changed,
                    focus_changed: outcome.focus_changed,
                    reason: outcome.reason.map(|reason| match reason {
                        PaneZoomNoopReason::SinglePane => PaneZoomReason::SinglePane,
                        PaneZoomNoopReason::AlreadyZoomed => PaneZoomReason::AlreadyZoomed,
                        PaneZoomNoopReason::AlreadyUnzoomed => PaneZoomReason::AlreadyUnzoomed,
                    }),
                    pane_id: pane_public_id,
                    focused_pane_id,
                    zoomed: outcome.zoomed,
                    layout,
                },
            },
        )
    }

    pub(super) fn handle_pane_input_set(
        &mut self,
        id: String,
        params: PaneInputSetParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(pane) = self
            .state
            .workspaces
            .get_mut(ws_idx)
            .and_then(|workspace| workspace.pane_state_mut(pane_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        pane.right_click_passthrough = matches!(
            params.right_click,
            crate::api::schema::PaneRightClickTarget::Pane
        );
        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_rename(&mut self, id: String, params: PaneRenameParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let normalized_label = params.label.map(|label| label.trim().to_string());
        match normalized_label.clone() {
            Some(label) if !label.is_empty() => terminal.set_manual_label(label),
            _ => terminal.clear_manual_label(),
        }
        let write_target = terminal.session_name_write_target().cloned();
        if let (Some(target), Some(label)) = (write_target, normalized_label.as_deref()) {
            if let Err(error) = crate::work_title::append_session_name(&target, label) {
                tracing::warn!(
                    %error,
                    terminal_id = %terminal_id,
                    "failed to write pane name to agent session"
                );
            }
        }
        self.state.mark_session_dirty();
        let pane = self.pane_info(ws_idx, pane_id).unwrap();

        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_work_context_set(
        &mut self,
        id: String,
        params: PaneWorkContextSetParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let mut patch = params.patch;
        if patch.active_owner == Some(true) {
            let requested_pr = self
                .state
                .terminals
                .get(&terminal_id)
                .ok_or_else(|| "pane terminal not found".to_string())
                .and_then(|terminal| {
                    let mut candidate = terminal.work_context.clone();
                    candidate.apply_manual_patch(patch.clone())?;
                    Ok(candidate.effective().primary_pr().map(str::to_string))
                });
            let requested_pr = match requested_pr {
                Ok(requested_pr) => requested_pr,
                Err(message) => return encode_error(id, "invalid_work_context", message),
            };
            if requested_pr.as_deref().is_some_and(|pr_url| {
                self.state.workspaces.iter().any(|workspace| {
                    workspace.tabs.iter().any(|tab| {
                        tab.panes.values().any(|pane| {
                            pane.attached_terminal_id != terminal_id
                                && self
                                    .state
                                    .terminals
                                    .get(&pane.attached_terminal_id)
                                    .map(crate::terminal::TerminalState::effective_work_context)
                                    .is_some_and(|existing| existing.is_active_owner_of(pr_url))
                        })
                    })
                })
            }) {
                patch.active_owner = Some(false);
            }
        }
        let mutation = self
            .state
            .terminals
            .get_mut(&terminal_id)
            .ok_or_else(|| "pane terminal not found".to_string())
            .and_then(|terminal| terminal.apply_manual_work_context_patch(patch));
        let changed = match mutation {
            Ok(changed) => changed,
            Err(message) => return encode_error(id, "invalid_work_context", message),
        };
        let mut ws_idx = ws_idx;
        if changed {
            self.state.mark_session_dirty();
            self.emit_pane_updated(ws_idx, pane_id);
            // A declaration is the strongest repository evidence there is, so
            // act on it immediately. The move never takes focus and never
            // touches the pane the human is in.
            self.route_pane_to_bound_workspace(ws_idx, pane_id);
            ws_idx = self.workspace_index_for_pane(pane_id).unwrap_or(ws_idx);
        }
        let Some(pane) = self.pane_info(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        encode_success(id, ResponseResult::PaneInfo { pane })
    }

    pub(super) fn handle_pane_read(&mut self, id: String, params: PaneReadParams) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent_session = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(crate::app::creation::terminal_agent_session_info);
        let agent_ref = agent_session.as_ref().and_then(|_| {
            crate::api::schema::AgentRef::new(
                self.state.agent_host_name.clone(),
                public_pane_id.clone(),
            )
            .ok()
        });
        let Some((pane, workspace_id)) = self.lookup_runtime(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(tab_idx) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|ws| ws.find_tab_index_for_pane(pane_id))
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let observation = (params.source == crate::api::schema::ReadSource::Detection
            && params.format == crate::api::schema::ReadFormat::Text
            && params.lines.is_none())
        .then(|| pane.input_observation())
        .flatten();
        let snapshot = observation.as_ref().map_or_else(
            || {
                crate::app::api_helpers::read_terminal_snapshot(
                    pane,
                    params.source,
                    params.format,
                    params.lines,
                )
            },
            |observation| crate::pane::TerminalReadSnapshot {
                text: observation.text.clone(),
                truncated: false,
            },
        );

        encode_success(
            id,
            ResponseResult::PaneRead {
                read: PaneReadResult {
                    pane_id: public_pane_id,
                    terminal_id: terminal_id.to_string(),
                    workspace_id,
                    tab_id: self.public_tab_id(ws_idx, tab_idx).unwrap(),
                    agent_ref,
                    agent_session,
                    source: params.source,
                    format: params.format,
                    text: snapshot.text,
                    revision: pane.content_revision(),
                    truncated: snapshot.truncated,
                    input_observation: observation.map(|observation| {
                        crate::api::schema::PaneInputObservation {
                            token: observation.token,
                        }
                    }),
                },
            },
        )
    }

    pub(super) fn handle_pane_report_agent(
        &mut self,
        id: String,
        params: PaneReportAgentParams,
    ) -> String {
        // A report from another closing-block wire version is skipped whole and
        // silently: mismatched versions must degrade to a no-op, never to an
        // error or a misparsed state change.
        if params
            .v
            .is_some_and(|version| version != crate::api::schema::panes::CLOSING_BLOCK_VERSION)
        {
            return encode_success(id, ResponseResult::Ok {});
        }
        let parsed_pane = self.parse_pane_id(&params.pane_id);
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            if parsed_pane.is_none() {
                return pane_not_found(id, &params.pane_id);
            }
            return invalid_agent(id);
        };
        let (ws_idx, pane_id) = match self.resolve_agent_report_pane(
            parsed_pane,
            &agent_label,
            params.agent_session_id.as_deref(),
        ) {
            AgentReportPaneResolution::Pane(target) => target,
            AgentReportPaneResolution::Missing => return pane_not_found(id, &params.pane_id),
            AgentReportPaneResolution::Ignore => return encode_success(id, ResponseResult::Ok {}),
        };
        let workers_unknown = params.workers_unknown;
        let completion = params.completion;
        let external_wait = params.external_wait.and_then(|wait| {
            let wait = wait
                .trim()
                .chars()
                .filter(|character| !character.is_control())
                .take(80)
                .collect::<String>();
            (!wait.is_empty()).then_some(wait)
        });
        let parse_status = params.parse_status;
        let dependencies_authoritative = !matches!(
            parse_status,
            Some(
                crate::api::schema::ClosingParseStatus::Missing
                    | crate::api::schema::ClosingParseStatus::Malformed
            )
        );
        let reported_external_wait = dependencies_authoritative
            .then(|| external_wait.clone())
            .flatten();
        let task_reported = completion.is_some()
            || external_wait.is_some()
            || parse_status.is_some()
            || workers_unknown.is_some();
        let gates = params.gates;
        let items = params.items;
        let decisions = params.decisions;
        let has_complete_arrays = gates.is_some() && items.is_some() && decisions.is_some();
        let has_malformed_blockers = matches!(
            parse_status,
            Some(crate::api::schema::ClosingParseStatus::Malformed)
        ) && (gates.as_ref().is_some_and(|gates| !gates.is_empty())
            || items.as_ref().is_some_and(|items| !items.is_empty()));
        let has_arrays = has_complete_arrays || has_malformed_blockers;
        let report_wait = if dependencies_authoritative {
            params.wait.or_else(|| reported_external_wait.clone())
        } else {
            None
        };
        let terminal_id = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone());
        let legacy_v2_closing_report = params.agent_session_id.is_none()
            && params.v == Some(crate::api::schema::panes::CLOSING_BLOCK_VERSION)
            && crate::detect::is_closing_block_source(&params.source, &agent_label);
        let legacy_session_id = if legacy_v2_closing_report {
            let correlation = terminal_id
                .as_ref()
                .and_then(|terminal_id| self.state.terminals.get(terminal_id))
                .map_or(Ok(None), |terminal| {
                    terminal.correlate_legacy_closing_report(
                        &params.source,
                        &agent_label,
                        params.seq,
                    )
                });
            match correlation {
                Ok(session_id) => session_id,
                Err(()) => return encode_success(id, ResponseResult::Ok {}),
            }
        } else {
            None
        };
        if params
            .agent_session_id
            .as_deref()
            .is_some_and(|session_id| {
                terminal_id
                    .as_ref()
                    .and_then(|terminal_id| self.state.terminals.get(terminal_id))
                    .is_none_or(|terminal| {
                        !terminal.agent_session_id_matches_current_agent(&agent_label, session_id)
                    })
            })
        {
            return encode_success(id, ResponseResult::Ok {});
        }
        let report_wait = report_wait.or_else(|| {
            (!dependencies_authoritative)
                .then(|| {
                    terminal_id
                        .as_ref()
                        .and_then(|terminal_id| self.state.terminals.get(terminal_id))
                        .and_then(|terminal| terminal.closing_external_wait().map(str::to_string))
                })
                .flatten()
        });
        let closing_block = (params.v == Some(crate::api::schema::panes::CLOSING_BLOCK_VERSION)
            && (has_arrays || task_reported))
            .then(|| {
                let (gates, items, decisions) = if dependencies_authoritative {
                    gates
                        .zip(items)
                        .zip(decisions)
                        .map(|((gates, items), decisions)| (gates, items, decisions))
                        .unwrap_or_default()
                } else {
                    (
                        gates.unwrap_or_default(),
                        items.unwrap_or_default(),
                        Vec::new(),
                    )
                };
                Box::new(crate::events::ClosingBlockReport {
                    gates,
                    items,
                    decisions,
                    agents: (workers_unknown != Some(true))
                        .then_some(params.agents)
                        .flatten(),
                    completion,
                    external_wait: external_wait.clone(),
                    parse_status,
                    workers_unknown,
                    settle_ready: params.settle_ready == Some(true),
                    dependencies_authoritative,
                    session_id: params
                        .agent_session_id
                        .clone()
                        .or_else(|| legacy_session_id.clone()),
                    last_turn_at: params.last_turn_at,
                })
            });
        let reported_state = detect_state_from_api(params.state);
        let report_state =
            if report_wait.is_some() && reported_state != crate::detect::AgentState::Blocked {
                crate::detect::AgentState::Working
            } else {
                reported_state
            };
        self.handle_internal_event(crate::events::AppEvent::HookStateReported {
            pane_id,
            session_ref: crate::agent_resume::session_ref_from_report(
                &params.source,
                &agent_label,
                params.agent_session_id,
                params.agent_session_path,
            ),
            source: params.source,
            agent_label,
            state: report_state,
            message: params.message,
            seq: params.seq,
            wait: report_wait,
            eta_s: params.eta_s,
            reported_at: params.reported_at,
            closing_block,
        });
        let _ = self.sync_terminal_titles();

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_report_agent_session(
        &mut self,
        id: String,
        params: PaneReportAgentSessionParams,
    ) -> String {
        let parsed_pane = self.parse_pane_id(&params.pane_id);
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            if parsed_pane.is_none() {
                return pane_not_found(id, &params.pane_id);
            }
            return invalid_agent(id);
        };
        let (ws_idx, pane_id) = match self.resolve_agent_report_pane(
            parsed_pane,
            &agent_label,
            params.agent_session_id.as_deref(),
        ) {
            AgentReportPaneResolution::Pane(target) => target,
            AgentReportPaneResolution::Missing => return pane_not_found(id, &params.pane_id),
            AgentReportPaneResolution::Ignore => return encode_success(id, ResponseResult::Ok {}),
        };
        let legacy_guard = (
            params.source.clone(),
            agent_label.clone(),
            params.agent_session_id.clone(),
            params.seq,
        );
        let closing_precursor_has_native_session =
            crate::detect::is_closing_block_source(&params.source, &agent_label)
                && self.state.workspaces[ws_idx]
                    .pane_state(pane_id)
                    .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
                    .is_some_and(|terminal| terminal.current_agent_session_id().is_some());
        if closing_precursor_has_native_session {
            if let Some(terminal) = self.state.workspaces[ws_idx]
                .pane_state(pane_id)
                .and_then(|pane| self.state.terminals.get_mut(&pane.attached_terminal_id))
            {
                terminal.record_legacy_closing_report_precursor(
                    legacy_guard.0,
                    legacy_guard.1,
                    legacy_guard.2,
                    legacy_guard.3,
                );
            }
            return encode_success(id, ResponseResult::Ok {});
        }
        let session_ref = crate::agent_resume::session_ref_from_report(
            &params.source,
            &agent_label,
            params.agent_session_id,
            params.agent_session_path.clone(),
        );
        let claude_transcript_path = crate::app::claude_subagents::validated_transcript_path(
            &params.source,
            &agent_label,
            session_ref.as_ref(),
            params.agent_session_path.as_deref(),
        );
        let session_name_provider = match (params.source.as_str(), agent_label.as_str()) {
            ("herdr:claude", "claude") => Some(crate::work_title::WorkTitleProvider::Claude),
            ("herdr:codex", "codex") => Some(crate::work_title::WorkTitleProvider::Codex),
            _ => None,
        };
        let existing_write_target = self.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .and_then(|terminal| terminal.session_name_write_target())
            .cloned();
        let session_name_write_target = match (session_name_provider, session_ref.as_ref()) {
            (Some(provider), Some(session_ref))
                if session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id =>
            {
                if provider == crate::work_title::WorkTitleProvider::Codex
                    && params.agent_session_path.is_none()
                    && existing_write_target.as_ref().is_some_and(|target| {
                        target.matches_provider_session(provider, &session_ref.value)
                    })
                {
                    existing_write_target.clone()
                } else {
                    let candidate_path = match provider {
                        crate::work_title::WorkTitleProvider::Claude => {
                            claude_transcript_path.clone()
                        }
                        crate::work_title::WorkTitleProvider::Codex => params
                            .agent_session_path
                            .as_deref()
                            .map(std::path::PathBuf::from),
                    };
                    candidate_path.and_then(|path| {
                        if existing_write_target.as_ref().is_some_and(|target| {
                            target.matches_binding(provider, &session_ref.value, &path)
                        }) {
                            return existing_write_target.clone();
                        }
                        let path = match provider {
                            crate::work_title::WorkTitleProvider::Claude => Some(path),
                            crate::work_title::WorkTitleProvider::Codex => {
                                crate::work_title::validated_codex_session_index_path(
                                    path.to_str(),
                                    &session_ref.value,
                                )
                            }
                        }?;
                        Some(crate::work_title::SessionNameWriteTarget::new(
                            provider,
                            session_ref.value.clone(),
                            path,
                        ))
                    })
                }
            }
            _ => None,
        };
        let session_name_target_evaluated = match session_name_provider {
            Some(crate::work_title::WorkTitleProvider::Claude) => {
                params.agent_session_path.is_some()
            }
            Some(crate::work_title::WorkTitleProvider::Codex) => {
                session_ref.as_ref().is_some_and(|session| {
                    session.kind == crate::agent_resume::AgentSessionRefKind::Id
                })
            }
            None => false,
        };
        self.handle_internal_event(crate::events::AppEvent::AgentSessionReported {
            pane_id,
            session_ref,
            claude_transcript_path,
            session_name_write_target,
            session_name_target_evaluated,
            source: params.source,
            agent_label,
            seq: params.seq,
            session_start_source: crate::agent_resume::normalize_session_start_source(
                params.session_start_source,
            ),
        });
        if let Some(terminal) = self.state.workspaces[ws_idx]
            .pane_state(pane_id)
            .and_then(|pane| self.state.terminals.get_mut(&pane.attached_terminal_id))
        {
            terminal.record_legacy_closing_report_precursor(
                legacy_guard.0,
                legacy_guard.1,
                legacy_guard.2,
                legacy_guard.3,
            );
        }
        let _ = self.sync_terminal_titles();

        encode_success(id, ResponseResult::Ok {})
    }

    fn resolve_agent_report_pane(
        &self,
        parsed_pane: Option<(usize, PaneId)>,
        agent_label: &str,
        session_id: Option<&str>,
    ) -> AgentReportPaneResolution {
        let Some((ws_idx, pane_id)) = parsed_pane else {
            let Some(session_id) = session_id else {
                return AgentReportPaneResolution::Missing;
            };
            if let UniqueAgentSessionPane::One(target) =
                self.unique_live_agent_session_pane(agent_label, session_id)
            {
                return AgentReportPaneResolution::Pane(target);
            }
            return match self.unique_agent_session_pane(agent_label, session_id) {
                UniqueAgentSessionPane::One(target) => AgentReportPaneResolution::Pane(target),
                UniqueAgentSessionPane::None | UniqueAgentSessionPane::Several => {
                    AgentReportPaneResolution::Missing
                }
            };
        };
        let Some(session_id) = session_id else {
            return AgentReportPaneResolution::Pane((ws_idx, pane_id));
        };

        let live_authority = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .and_then(crate::terminal::TerminalState::effective_non_retired_hook_authority);
        let live_session_matches = live_authority.is_some_and(|authority| {
            authority.agent_label == agent_label
                && authority.session_ref.as_ref().is_some_and(|session_ref| {
                    session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                        && session_ref.value == session_id
                })
        });
        if live_session_matches {
            return AgentReportPaneResolution::Pane((ws_idx, pane_id));
        }

        let live_conflict = live_authority.is_some_and(|authority| {
            authority.agent_label != agent_label
                || authority.session_ref.as_ref().is_some_and(|session_ref| {
                    session_ref.kind != crate::agent_resume::AgentSessionRefKind::Id
                        || session_ref.value != session_id
                })
        });
        let persisted_match = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .and_then(|pane| self.state.terminals.get(&pane.attached_terminal_id))
            .is_some_and(|terminal| {
                Self::persisted_agent_session_matches(terminal, agent_label, session_id)
            });
        if live_conflict || persisted_match {
            if let UniqueAgentSessionPane::One(target) =
                self.unique_live_agent_session_pane(agent_label, session_id)
            {
                if target != (ws_idx, pane_id) {
                    return AgentReportPaneResolution::Pane(target);
                }
            }
        }
        if live_conflict {
            return AgentReportPaneResolution::Ignore;
        }

        // A resolved pane with no live conflicting session can be a shell, a
        // newly started agent, or a resumed session whose first hook is still
        // reporting. Persisted identity on another pane must not steal it.
        AgentReportPaneResolution::Pane((ws_idx, pane_id))
    }

    fn persisted_agent_session_matches(
        terminal: &crate::terminal::TerminalState,
        agent_label: &str,
        session_id: &str,
    ) -> bool {
        terminal
            .persisted_agent_session
            .as_ref()
            .is_some_and(|session| {
                session.agent == agent_label
                    && session.session_ref.kind == crate::agent_resume::AgentSessionRefKind::Id
                    && session.session_ref.value == session_id
            })
    }

    fn unique_agent_session_pane(
        &self,
        agent_label: &str,
        session_id: &str,
    ) -> UniqueAgentSessionPane {
        let mut owner = None;
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (&pane_id, pane) in &tab.panes {
                    let matches = self
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .is_some_and(|terminal| {
                            terminal.agent_session_id_matches_current_agent(agent_label, session_id)
                                || Self::persisted_agent_session_matches(
                                    terminal,
                                    agent_label,
                                    session_id,
                                )
                        });
                    if matches {
                        if owner.is_some() {
                            return UniqueAgentSessionPane::Several;
                        }
                        owner = Some((ws_idx, pane_id));
                    }
                }
            }
        }
        owner.map_or(UniqueAgentSessionPane::None, UniqueAgentSessionPane::One)
    }

    fn unique_live_agent_session_pane(
        &self,
        agent_label: &str,
        session_id: &str,
    ) -> UniqueAgentSessionPane {
        let mut owner = None;
        for (ws_idx, workspace) in self.state.workspaces.iter().enumerate() {
            for tab in &workspace.tabs {
                for (&pane_id, pane) in &tab.panes {
                    let matches = self
                        .state
                        .terminals
                        .get(&pane.attached_terminal_id)
                        .and_then(
                            crate::terminal::TerminalState::effective_non_retired_hook_session_id,
                        )
                        .is_some_and(|(current_agent, current_session)| {
                            current_agent == agent_label && current_session == session_id
                        });
                    if matches {
                        if owner.is_some() {
                            return UniqueAgentSessionPane::Several;
                        }
                        owner = Some((ws_idx, pane_id));
                    }
                }
            }
        }
        owner.map_or(UniqueAgentSessionPane::None, UniqueAgentSessionPane::One)
    }

    pub(super) fn handle_pane_report_metadata(
        &mut self,
        id: String,
        params: PaneReportMetadataParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let agent_label = match params.agent.as_deref() {
            Some(agent) => match normalize_reported_agent_label(agent) {
                Some(agent_label) => Some(agent_label),
                None => return invalid_agent(id),
            },
            None => None,
        };
        let source = match normalize_metadata_source(params.source) {
            Ok(source) => source,
            Err(message) => return encode_error(id, "invalid_metadata_source", message),
        };
        let raw_title_set = params.title.is_some();
        let raw_display_agent_set = params.display_agent.is_some();
        let raw_state_labels_set = !params.state_labels.is_empty();
        let mut tokens = if params.tokens.is_empty() {
            None
        } else {
            match normalize_metadata_tokens(params.tokens) {
                Ok(tokens) => Some(tokens),
                Err(message) => return encode_error(id, "invalid_metadata_token", message),
            }
        };
        let ttl = match normalize_metadata_ttl(params.ttl_ms) {
            Ok(ttl) => ttl,
            Err(message) => return encode_error(id, "invalid_metadata_ttl", message),
        };
        let mut title = normalize_presentation_text(params.title);
        let requested_work_title = (source == crate::work_title::WORK_TITLE_SOURCE)
            .then(|| title.clone())
            .flatten();
        let requested_hook_context = match params.work_context {
            Some(context) => match context.normalized() {
                Ok(context) => Some(context),
                Err(message) => return encode_error(id, "invalid_work_context", message),
            },
            None => None,
        };
        let display_agent = normalize_presentation_text(params.display_agent);
        let applies_to_source = match params.applies_to_source {
            Some(applies_to_source) => match normalize_metadata_source(applies_to_source) {
                Ok(applies_to_source) => Some(applies_to_source),
                Err(message) => return encode_error(id, "invalid_metadata_source", message),
            },
            None => None,
        };
        let raw_agent_session_id_set = params.agent_session_id.is_some();
        let agent_session_id = params
            .agent_session_id
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if raw_agent_session_id_set && agent_session_id.is_none() {
            return encode_error(
                id,
                "invalid_agent_session",
                "agent_session_id must contain visible text",
            );
        }
        let work_title_request = source == crate::work_title::WORK_TITLE_SOURCE
            && agent_label.is_some()
            && applies_to_source.is_some()
            && agent_session_id.is_some();
        // A session rename carries the agent's own name for the session and
        // nothing else. It is guarded exactly like a work title, but it patches
        // the session name instead of replacing the hook work context.
        let session_name_request = source == crate::work_title::SESSION_NAME_SOURCE
            && agent_label.is_some()
            && applies_to_source.is_some()
            && agent_session_id.is_some();
        if let Some(context) = requested_hook_context.as_ref() {
            // `repo` is refused on both hook shapes. The hook context is
            // derived from prompt text, and a repository named in prose is
            // ambient rather than evidence; letting it through would hand a
            // prose mention the precedence of a declaration and misfile the
            // pane. Repositories arrive by declaration or by observation only.
            let valid_work_title_context = work_title_request
                && context.branch.is_none()
                && context.repo.is_none()
                && context.session_name.is_none()
                && context.work_title == requested_work_title;
            let valid_session_name_context = session_name_request
                && context.session_name.as_deref().is_some_and(|name| {
                    crate::work_title::normalize_session_name_for_write(name).is_some()
                })
                && context.work_title.is_none()
                && context.branch.is_none()
                && context.repo.is_none()
                && context.ticket_ids.is_empty()
                && context.pr_urls.is_empty()
                && context.preview_urls.is_empty()
                && context.missive_urls.is_empty();
            if !valid_work_title_context && !valid_session_name_context {
                return encode_error(
                    id,
                    "invalid_work_context",
                    "derived work context requires matching guarded work-title metadata",
                );
            }
        } else if session_name_request {
            return encode_error(
                id,
                "invalid_work_context",
                "session rename requires a session name",
            );
        }
        if agent_session_id.is_some() && (agent_label.is_none() || applies_to_source.is_none()) {
            return encode_error(
                id,
                "invalid_metadata_request",
                "agent_session_id requires agent and applies_to_source guards",
            );
        }
        let state_labels = match normalize_state_labels(params.state_labels) {
            Ok(labels) => labels,
            Err(status) => {
                return encode_error(
                    id,
                    "invalid_state_label",
                    format!("unknown state label: {status}"),
                );
            }
        };
        if raw_title_set && params.clear_title
            || raw_display_agent_set && params.clear_display_agent
            || raw_state_labels_set && params.clear_state_labels
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "cannot set and clear the same metadata field",
            );
        }
        if title.is_none()
            && display_agent.is_none()
            && state_labels.is_empty()
            && tokens.is_none()
            && !params.clear_title
            && !params.clear_display_agent
            && !params.clear_state_labels
            && !work_title_request
            && !session_name_request
        {
            return encode_error(
                id,
                "invalid_metadata_request",
                "missing metadata field to set or clear",
            );
        }
        let presentation_requested = title.is_some()
            || display_agent.is_some()
            || !state_labels.is_empty()
            || params.clear_title
            || params.clear_display_agent
            || params.clear_state_labels;
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.pane_state(pane_id))
            .map(|pane| pane.attached_terminal_id.clone())
        else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(terminal) = self.state.terminals.get_mut(&terminal_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        if let Some(agent_session_id) = agent_session_id.as_deref() {
            let session_matches = agent_label
                .as_deref()
                .zip(applies_to_source.as_deref())
                .is_some_and(|(agent, source)| {
                    if crate::detect::is_closing_block_source(source, agent) {
                        terminal.agent_session_id_matches_current_agent(agent, agent_session_id)
                    } else {
                        terminal.agent_session_matches(source, agent, agent_session_id)
                    }
                });
            if !session_matches {
                return encode_error(
                    id,
                    "agent_session_mismatch",
                    "metadata report does not match the pane's current agent session",
                );
            }
        }
        if terminal.metadata_report_blocked_by_process_exit(
            &source,
            agent_label.as_deref(),
            applies_to_source.as_deref(),
        ) {
            return encode_success(id, ResponseResult::Ok {});
        }
        let closing_agent = terminal
            .effective_agent_label()
            .filter(|agent| crate::detect::is_closing_block_source(&source, agent));
        let closing_block_metadata =
            applies_to_source.as_deref() == Some(source.as_str()) && closing_agent.is_some();
        if closing_block_metadata && agent_session_id.is_none() {
            let metadata_matches = closing_agent.is_some_and(|agent| {
                terminal.accepts_legacy_closing_metadata(&source, agent, params.seq)
            });
            if !metadata_matches {
                return encode_success(id, ResponseResult::Ok {});
            }
        }
        if closing_block_metadata {
            if let Some(tokens) = tokens.as_mut() {
                if tokens.contains_key("closing_idle") {
                    tokens.entry("closing_contract".into()).or_insert(None);
                    tokens.entry("closing_contract_met".into()).or_insert(None);
                }
                let parse = tokens
                    .get("closing_parse")
                    .and_then(Option::as_deref)
                    .map(str::to_string);
                if matches!(parse.as_deref(), Some("missing" | "malformed")) {
                    for key in [
                        "closing_agents",
                        "closing_agent_names",
                        "closing_wait",
                        "closing_workers_unknown",
                    ] {
                        tokens.remove(key);
                    }
                }
                if tokens
                    .get("closing_workers_unknown")
                    .and_then(Option::as_deref)
                    == Some("1")
                {
                    tokens.remove("closing_agents");
                }
                if parse.as_deref() == Some("missing") {
                    tokens.remove("closing_blocking");
                    tokens.remove("closing_gates");
                } else if parse.as_deref() == Some("malformed") {
                    let blocking = terminal.closing_gates().len().saturating_add(
                        terminal
                            .closing_items()
                            .iter()
                            .filter(|item| item.requires_human_input())
                            .count(),
                    );
                    let gates = terminal
                        .closing_gates()
                        .iter()
                        .map(|gate| gate.text.as_str())
                        .collect::<Vec<_>>()
                        .join("; ")
                        .chars()
                        .take(200)
                        .collect::<String>();
                    tokens.insert("closing_blocking".into(), Some(blocking.to_string()));
                    tokens.insert("closing_gates".into(), (!gates.is_empty()).then_some(gates));
                }
            }
        }
        if closing_block_metadata
            && !terminal.closing_metadata_not_older_than_report(&source, params.seq)
        {
            return encode_success(id, ResponseResult::Ok {});
        }
        if !terminal.metadata_report_sequence_is_fresh(&source, params.seq) {
            return encode_success(id, ResponseResult::Ok {});
        }
        let metadata_agent = crate::terminal::TerminalState::metadata_report_agent(
            &source,
            agent_label.as_deref(),
            applies_to_source.as_deref(),
        );
        if let Some(tokens) = tokens.as_ref() {
            if terminal.metadata_tokens.key_count_after_patch(tokens)
                > MAX_METADATA_TOKEN_KEYS_PER_RESOURCE
            {
                return encode_error(
                    id,
                    "metadata_token_limit",
                    format!(
                        "pane metadata may contain at most {MAX_METADATA_TOKEN_KEYS_PER_RESOURCE} tokens"
                    ),
                );
            }
        }
        match terminal.accept_metadata_report(&source, params.seq, tokens.is_some(), metadata_agent)
        {
            Ok(true) => {}
            Ok(false) => return encode_success(id, ResponseResult::Ok {}),
            Err(()) => {
                return encode_error(
                    id,
                    "metadata_sequence_source_limit",
                    format!(
                        "pane metadata may track at most {} sequenced sources",
                        crate::metadata_tokens::MAX_SEQUENCE_SOURCES
                    ),
                );
            }
        }
        let work_title = match (
            work_title_request,
            agent_label.as_deref(),
            applies_to_source.as_deref(),
            agent_session_id.as_deref(),
        ) {
            (true, Some(agent), Some(lifecycle_source), Some(session_id)) => {
                let has_new_reference = requested_hook_context.as_ref().is_some_and(|requested| {
                    crate::work_title::work_title_has_new_reference(
                        terminal.effective_work_context(),
                        requested,
                    )
                });
                let work_title = terminal.resolve_work_title_for_session(
                    agent,
                    lifecycle_source,
                    session_id,
                    requested_work_title,
                    params.work_title_long_brief || has_new_reference,
                );
                title = work_title.clone();
                work_title
            }
            _ => None,
        };
        let session_name_changed = if session_name_request {
            let session_name = requested_hook_context
                .as_ref()
                .and_then(|context| context.session_name.clone());
            match terminal.set_hook_session_name(session_name) {
                Ok(changed) => changed,
                Err(message) => return encode_error(id, "invalid_work_context", message),
            }
        } else {
            false
        };
        let hook_context_changed = if work_title_request {
            let branch = terminal.effective_work_context().branch.clone();
            let context = match crate::work_context::hook_turn_context(
                work_title.clone(),
                branch.as_deref(),
                requested_hook_context.unwrap_or_default(),
            ) {
                Ok(context) => context,
                Err(message) => return encode_error(id, "invalid_work_context", message),
            };
            match terminal.replace_hook_work_context(context) {
                Ok(changed) => changed,
                Err(message) => return encode_error(id, "invalid_work_context", message),
            }
        } else {
            false
        };
        let unchanged_title_only = title.as_ref().is_some_and(|title| {
            terminal
                .agent_metadata
                .get(&source)
                .is_some_and(|metadata| {
                    metadata.title.as_ref() == Some(title)
                        && metadata.agent_label == agent_label
                        && metadata.applies_to_source == applies_to_source
                })
        }) && display_agent.is_none()
            && state_labels.is_empty()
            && tokens.is_none()
            && !params.clear_title
            && !params.clear_display_agent
            && !params.clear_state_labels
            && ttl.is_none()
            && !hook_context_changed
            && !session_name_changed;
        if unchanged_title_only {
            return encode_success(id, ResponseResult::Ok {});
        }
        let now = std::time::Instant::now();
        let closing_contract_changed = tokens.as_ref().is_some_and(|tokens| {
            closing_block_metadata && terminal.apply_closing_contract_tokens(tokens, now)
        });
        if closing_block_metadata {
            if let Some(tokens) = tokens.as_mut() {
                tokens.retain(|key, _| !key.starts_with("closing_"));
            }
        }
        let token_changed = tokens.is_some_and(|tokens| {
            let changed = terminal.metadata_tokens.patch(tokens, ttl, now);
            if changed {
                terminal.revision = terminal.revision.saturating_add(1);
            }
            changed
        });

        if presentation_requested {
            self.handle_internal_event(crate::events::AppEvent::HookMetadataReported {
                pane_id,
                source,
                agent_label,
                applies_to_source,
                title,
                display_agent,
                state_labels,
                clear_title: params.clear_title,
                clear_display_agent: params.clear_display_agent,
                clear_state_labels: params.clear_state_labels,
                seq: None,
                ttl,
            });
        }
        if token_changed {
            self.sync_agent_metadata_deadline();
        }
        if hook_context_changed || session_name_changed {
            self.schedule_session_save();
        }
        if token_changed || closing_contract_changed || hook_context_changed || session_name_changed
        {
            self.emit_pane_updated(ws_idx, pane_id);
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_clear_agent_authority(
        &mut self,
        id: String,
        params: PaneClearAgentAuthorityParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAuthorityCleared {
            pane_id,
            source: params.source,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_release_agent(
        &mut self,
        id: String,
        params: PaneReleaseAgentParams,
    ) -> String {
        let Some((_ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(agent_label) = normalize_reported_agent_label(&params.agent) else {
            return invalid_agent(id);
        };
        self.handle_internal_event(crate::events::AppEvent::HookAgentReleased {
            pane_id,
            source: params.source,
            known_agent: crate::detect::parse_agent_label(&agent_label),
            agent_label,
            seq: params.seq,
        });

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_text(
        &mut self,
        id: String,
        params: PaneSendTextParams,
    ) -> String {
        match self.try_send_text_to_pane(&params.pane_id, &params.text) {
            Ok(()) => {}
            Err(PaneSendError::NotFound) => return pane_not_found(id, &params.pane_id),
            Err(PaneSendError::Failed(error)) => {
                return encode_error(id, "pane_send_failed", error);
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_send_text_if(
        &mut self,
        id: String,
        params: PaneSendTextIfParams,
    ) -> String {
        let mismatch = || {
            encode_success(
                id.clone(),
                ResponseResult::PaneTextSend {
                    outcome: PaneSendTextIfOutcome::ConditionMismatch,
                },
            )
        };
        let Some((ws_idx, pane_id)) = self.parse_current_public_pane_id(&params.pane_id) else {
            return mismatch();
        };
        if self.public_workspace_id(ws_idx) != params.workspace_id {
            return mismatch();
        }
        let Some(terminal_id) = self
            .state
            .workspaces
            .get(ws_idx)
            .and_then(|workspace| workspace.terminal_id(pane_id))
            .cloned()
        else {
            return mismatch();
        };
        if terminal_id.as_str() != params.terminal_id {
            return mismatch();
        }
        let Ok(agent_ref) = crate::api::schema::AgentRef::new(
            self.state.agent_host_name.clone(),
            params.pane_id.clone(),
        ) else {
            return mismatch();
        };
        if agent_ref != params.agent_ref {
            return mismatch();
        }
        let agent_session = self
            .state
            .terminals
            .get(&terminal_id)
            .and_then(crate::app::creation::terminal_agent_session_info);
        if agent_session.as_ref() != Some(&params.agent_session) {
            return mismatch();
        }
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return mismatch();
        };
        let outcome = runtime.try_send_bytes_if_observation(
            &params.observation_token,
            Bytes::copy_from_slice(params.text.as_bytes()),
        );
        match outcome {
            ConditionalInputResult::Sent => {
                if !params.text.is_empty() {
                    self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                }
                encode_success(
                    id,
                    ResponseResult::PaneTextSend {
                        outcome: PaneSendTextIfOutcome::Sent,
                    },
                )
            }
            ConditionalInputResult::ConditionMismatch => mismatch(),
            ConditionalInputResult::Failed => {
                encode_error(id, "pane_send_failed", "pane input queue unavailable")
            }
        }
    }

    pub(super) fn handle_pane_send_input(
        &mut self,
        id: String,
        params: PaneSendInputParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let bytes = match super::super::api_helpers::encode_api_input(
            runtime,
            &params.text,
            &params.keys,
        ) {
            Ok(bytes) => bytes,
            Err(key) => return encode_error(id, "invalid_key", format!("unsupported key {key}")),
        };
        let has_bytes = !bytes.is_empty();
        if let Err(err) = runtime.try_send_bytes(Bytes::from(bytes)) {
            return encode_error(id, "pane_send_failed", err.to_string());
        }
        if has_bytes {
            self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
        }

        encode_success(id, ResponseResult::Ok {})
    }

    pub(super) fn handle_pane_close(&mut self, id: String, target: PaneTarget) -> String {
        match self.close_pane(id.clone(), &target) {
            Ok(()) => encode_success(id, ResponseResult::Ok {}),
            Err(response) => response,
        }
    }

    /// Close a pane; `Err` carries the encoded error response.
    pub(super) fn close_pane(&mut self, id: String, target: &PaneTarget) -> Result<(), String> {
        self.close_pane_with_scope(id, target, false)
    }

    pub(crate) fn close_pane_for_reap(
        &mut self,
        id: String,
        target: &PaneTarget,
    ) -> Result<(), String> {
        self.close_pane_with_scope(id, target, true)
    }

    fn close_pane_with_scope(
        &mut self,
        id: String,
        target: &PaneTarget,
        exact_workspace: bool,
    ) -> Result<(), String> {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&target.pane_id) else {
            return Err(pane_not_found(id, &target.pane_id));
        };
        let Some(public_pane_id) = self.public_pane_id(ws_idx, pane_id) else {
            return Err(pane_not_found(id, &target.pane_id));
        };
        let workspace_id = self.public_workspace_id(ws_idx);
        self.fleet_attach_agents.remove(&pane_id);
        let layout_update_target = self.layout_update_target_after_pane_removal(ws_idx, pane_id);
        if !exact_workspace {
            let close_tab_idx = self.state.workspaces.get(ws_idx).and_then(|workspace| {
                let tab_idx = workspace.find_tab_index_for_pane(pane_id)?;
                let tab = workspace.tabs.get(tab_idx)?;
                let leaves_only_companions = tab
                    .panes
                    .iter()
                    .filter(|(candidate, _)| **candidate != pane_id)
                    .all(|(_, pane)| pane.is_companion);
                (tab.layout.pane_count() <= 1 || leaves_only_companions).then_some(tab_idx)
            });
            if let Some(tab_idx) = close_tab_idx {
                if self
                    .state
                    .workspaces
                    .get(ws_idx)
                    .is_some_and(|workspace| workspace.tabs.len() == 1)
                {
                    return self
                        .close_last_tab_workspace(ws_idx, tab_idx)
                        .map_err(|message| {
                            let code = if message.starts_with("confirmation_required:") {
                                "confirmation_required"
                            } else {
                                "pane_close_failed"
                            };
                            encode_error(id, code, message)
                        });
                }
                return self
                    .close_tab_preserving_workspace(ws_idx, tab_idx, true)
                    .map_err(|message| encode_error(id, "pane_close_failed", message));
            }
        }
        let workspace_snapshot = self.workspace_info(ws_idx);
        let terminal_id = self.state.terminal_id_for_pane(ws_idx, pane_id);
        let should_close_workspace = {
            let Some(ws) = self.state.workspaces.get_mut(ws_idx) else {
                return Err(pane_not_found(id, &target.pane_id));
            };
            ws.close_pane(pane_id)
        };
        self.state.remove_plugin_pane_records([pane_id]);
        if should_close_workspace {
            self.state.selected = ws_idx;
            if exact_workspace {
                self.state.close_workspace_exact(ws_idx);
            } else {
                self.state.close_selected_workspace();
            }
            self.shutdown_detached_terminal_runtimes();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id: workspace_id.clone(),
                },
            });
            self.emit_event(EventEnvelope {
                event: EventKind::WorkspaceClosed,
                data: EventData::WorkspaceClosed {
                    workspace_id,
                    workspace: Some(workspace_snapshot),
                },
            });
        } else {
            self.state.remove_unattached_terminal_ids(terminal_id);
            self.shutdown_detached_terminal_runtimes();
            self.schedule_session_save();
            self.emit_event(EventEnvelope {
                event: EventKind::PaneClosed,
                data: EventData::PaneClosed {
                    pane_id: public_pane_id,
                    workspace_id,
                },
            });
            if let Some((ws_idx, tab_idx)) = layout_update_target {
                self.emit_layout_updated_event(ws_idx, tab_idx);
            }
        }

        Ok(())
    }

    pub(super) fn handle_pane_send_keys(
        &mut self,
        id: String,
        params: PaneSendKeysParams,
    ) -> String {
        let Some((ws_idx, pane_id)) = self.parse_pane_id(&params.pane_id) else {
            return pane_not_found(id, &params.pane_id);
        };
        let encoded_keys = {
            let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                return pane_not_found(id, &params.pane_id);
            };
            match encode_api_keys(runtime, &params.keys) {
                Ok(encoded_keys) => encoded_keys,
                Err(key) => {
                    return encode_error(id, "invalid_key", format!("unsupported key {key}"));
                }
            }
        };
        for bytes in encoded_keys {
            let has_bytes = !bytes.is_empty();
            let send_result = {
                let Some(runtime) = self.lookup_runtime_sender(ws_idx, pane_id) else {
                    return pane_not_found(id, &params.pane_id);
                };
                runtime.try_send_bytes(Bytes::from(bytes))
            };
            if let Err(err) = send_result {
                return encode_error(id, "pane_send_failed", err.to_string());
            }
            if has_bytes {
                self.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
            }
        }

        encode_success(id, ResponseResult::Ok {})
    }
}

fn normalize_presentation_text(value: Option<String>) -> Option<String> {
    let trimmed = value?.trim().to_string();
    let normalized: String = trimmed
        .chars()
        .filter(|ch| !ch.is_control())
        .take(80)
        .collect();
    (!normalized.trim().is_empty()).then(|| normalized.trim().to_string())
}

fn normalize_state_labels(
    labels: std::collections::HashMap<String, String>,
) -> Result<std::collections::HashMap<String, String>, String> {
    labels
        .into_iter()
        .map(|(status, label)| {
            let status = status.trim().to_ascii_lowercase();
            if !matches!(
                status.as_str(),
                "idle" | "working" | "blocked" | "done" | "stale" | "unknown"
            ) {
                return Err(status);
            }
            Ok(normalize_presentation_text(Some(label)).map(|label| (status, label)))
        })
        .filter_map(Result::transpose)
        .collect()
}

fn pane_not_found(id: String, pane_id: &str) -> String {
    encode_error(id, "pane_not_found", format!("pane {pane_id} not found"))
}

impl App {
    fn resolve_optional_pane(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        match pane_id {
            Some(pane_id) => self.parse_pane_id(pane_id),
            None => {
                let ws_idx = self.state.active?;
                let pane_id = self.state.workspaces.get(ws_idx)?.focused_pane_id()?;
                Some((ws_idx, pane_id))
            }
        }
    }

    fn resolve_swap_source(&self, pane_id: Option<&str>) -> Option<(usize, PaneId)> {
        self.resolve_optional_pane(pane_id)
    }

    fn directional_pane_target(
        &self,
        ws_idx: usize,
        tab_idx: usize,
        source_pane_id: PaneId,
        direction: PaneDirection,
    ) -> Option<PaneId> {
        let tab = self.state.workspaces.get(ws_idx)?.tabs.get(tab_idx)?;
        let panes = tab.layout.panes(self.state.view.terminal_area);
        let source = panes.iter().find(|pane| pane.id == source_pane_id)?;
        find_in_direction(source, direction.into(), &panes)
    }

    pub(super) fn pane_layout_snapshot(
        &self,
        ws_idx: usize,
        tab_idx: usize,
    ) -> Option<PaneLayoutSnapshot> {
        let ws = self.state.workspaces.get(ws_idx)?;
        let tab = ws.tabs.get(tab_idx)?;
        let area = self.state.view.terminal_area;
        let focused_pane_id = self.public_pane_id(ws_idx, tab.layout.focused())?;
        let panes = crate::ui::apply_pane_chrome(
            tab.layout.panes(area),
            self.state.pane_borders,
            self.state.pane_gaps,
            self.state.pane_outer_borders,
        )
        .into_iter()
        .filter_map(|pane| {
            Some(PaneLayoutPane {
                pane_id: self.public_pane_id(ws_idx, pane.id)?,
                focused: pane.is_focused,
                rect: pane.rect.into(),
            })
        })
        .collect();
        let splits = tab
            .layout
            .splits(area)
            .into_iter()
            .enumerate()
            .map(|(idx, split)| PaneLayoutSplit {
                id: split_path_id(idx, &split.path),
                direction: match split.direction {
                    ratatui::layout::Direction::Horizontal => {
                        crate::api::schema::SplitDirection::Right
                    }
                    ratatui::layout::Direction::Vertical => {
                        crate::api::schema::SplitDirection::Down
                    }
                },
                ratio: split.ratio,
                rect: split.area.into(),
            })
            .collect();

        Some(PaneLayoutSnapshot {
            workspace_id: self.public_workspace_id(ws_idx),
            tab_id: self.public_tab_id(ws_idx, tab_idx)?,
            zoomed: tab.zoomed,
            area: area.into(),
            focused_pane_id,
            panes,
            splits,
        })
    }

    pub(crate) fn emit_layout_updated_event(&mut self, ws_idx: usize, tab_idx: usize) {
        if let Some(layout) = self.pane_layout_snapshot(ws_idx, tab_idx) {
            self.emit_layout_updated_snapshot(layout);
        }
    }

    pub(super) fn emit_layout_updated_snapshot(&mut self, layout: PaneLayoutSnapshot) {
        self.emit_event(EventEnvelope {
            event: EventKind::LayoutUpdated,
            data: EventData::LayoutUpdated { layout },
        });
    }

    pub(crate) fn layout_update_target_after_pane_removal(
        &self,
        ws_idx: usize,
        pane_id: PaneId,
    ) -> Option<(usize, usize)> {
        let tab_idx = self
            .state
            .workspaces
            .get(ws_idx)?
            .find_tab_index_for_pane(pane_id)?;
        let pane_count = self
            .state
            .workspaces
            .get(ws_idx)?
            .tabs
            .get(tab_idx)?
            .layout
            .pane_count();
        (pane_count > 1).then_some((ws_idx, tab_idx))
    }
}

impl From<PaneDirection> for NavDirection {
    fn from(direction: PaneDirection) -> Self {
        match direction {
            PaneDirection::Left => NavDirection::Left,
            PaneDirection::Right => NavDirection::Right,
            PaneDirection::Up => NavDirection::Up,
            PaneDirection::Down => NavDirection::Down,
        }
    }
}

enum ResolvedPaneMoveDestination {
    ExistingTab {
        tab_id: String,
        target_pane_id: PaneId,
        split: crate::api::schema::SplitDirection,
        ratio: f32,
        cross_workspace: bool,
    },
    NewTab {
        workspace_id: String,
        label: Option<String>,
    },
    NewWorkspace {
        label: Option<String>,
        tab_label: Option<String>,
    },
}

struct PaneMoveRecoveryContext {
    source_ws_idx: usize,
    previous_workspace_id: String,
    previous_workspace_label: Option<String>,
    previous_tab_label: Option<String>,
    previous_worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    /// A failed move must not silently drop the source workspace's repository
    /// binding; losing it would reintroduce the decay this feature prevents.
    previous_repo_binding: Option<String>,
    /// Restored alongside the binding: recreating the workspace without it
    /// would let adoption rebind a workspace the operator had cleared.
    previous_repo_binding_cleared: bool,
    identity_cwd: std::path::PathBuf,
}

fn encode_unchanged_pane_move(
    id: String,
    reason: PaneMoveReason,
    previous_pane_id: String,
    previous_workspace_id: String,
    previous_tab_id: String,
    pane: PaneInfo,
    source_layout: Option<PaneLayoutSnapshot>,
    target_layout: PaneLayoutSnapshot,
) -> String {
    let focused_pane_id = target_layout.focused_pane_id.clone();
    encode_success(
        id,
        ResponseResult::PaneMove {
            move_result: PaneMoveResult {
                changed: false,
                reason: Some(reason),
                previous_pane_id,
                previous_workspace_id,
                previous_tab_id,
                pane: Box::new(pane),
                source_layout: source_layout.map(Box::new),
                target_layout: Box::new(target_layout),
                created_workspace: None,
                created_tab: None,
                closed_workspace_id: None,
                closed_tab_id: None,
                focused_pane_id,
            },
        },
    )
}

fn split_direction_to_layout(
    direction: crate::api::schema::SplitDirection,
) -> (ratatui::layout::Direction, bool) {
    match direction {
        crate::api::schema::SplitDirection::Left => (ratatui::layout::Direction::Horizontal, true),
        crate::api::schema::SplitDirection::Right => {
            (ratatui::layout::Direction::Horizontal, false)
        }
        crate::api::schema::SplitDirection::Up => (ratatui::layout::Direction::Vertical, true),
        crate::api::schema::SplitDirection::Down => (ratatui::layout::Direction::Vertical, false),
    }
}

impl From<ratatui::layout::Rect> for PaneLayoutRect {
    fn from(rect: ratatui::layout::Rect) -> Self {
        Self {
            x: rect.x,
            y: rect.y,
            width: rect.width,
            height: rect.height,
        }
    }
}

fn split_path_id(idx: usize, path: &[bool]) -> String {
    if path.is_empty() {
        return format!("split_{idx}_root");
    }
    let path = path
        .iter()
        .map(|right| if *right { "1" } else { "0" })
        .collect::<Vec<_>>()
        .join("");
    format!("split_{idx}_{path}")
}

fn invalid_agent(id: String) -> String {
    encode_error(id, "invalid_agent", "agent label must not be empty")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{
        api::schema::{ErrorResponse, SplitDirection, SuccessResponse},
        config::Config,
        detect::{Agent, AgentState},
        workspace::Workspace,
    };

    fn app_with_test_workspace() -> (App, String) {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("metadata")];
        app.state.ensure_test_terminals();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        (app, public_pane_id)
    }

    fn add_test_workspace(app: &mut App, name: &str) -> String {
        let workspace_idx = app.state.workspaces.len();
        let workspace = Workspace::test_new(name);
        let pane_id = workspace.tabs[0].root_pane;
        app.state.workspaces.push(workspace);
        app.state.ensure_test_terminals();
        app.public_pane_id(workspace_idx, pane_id).unwrap()
    }

    fn session_name_temp_dir(label: &str) -> std::path::PathBuf {
        use std::sync::atomic::{AtomicU64, Ordering};
        static NEXT: AtomicU64 = AtomicU64::new(1);

        let path = std::env::temp_dir().join(format!(
            "herdr-session-name-{label}-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&path).unwrap();
        path
    }

    fn quiet_settle_test_app() -> (App, String, PaneId, crate::terminal::TerminalId) {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state.active = None;
        app.state.auto_settle_inactive = false;
        app.state.auto_settle_finished = false;
        app.state.auto_settle_done = true;
        app.state.settle_done_after = std::time::Duration::from_nanos(1);
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            session_ref: crate::agent_resume::AgentSessionRef::id("quiet-settle-test")
                .expect("valid session id"),
        });
        let old_quiet = std::time::Instant::now();
        let pane = app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap();
        pane.activity.set_last_at(old_quiet);
        pane.activity.observe_quiet(true, old_quiet);
        (app, public_pane_id, pane_id, terminal_id)
    }

    fn quiet_settle_ready_at(app: &App, pane_id: PaneId) -> std::time::Instant {
        app.state.workspaces[0].tabs[0].panes[&pane_id]
            .activity
            .last_at()
            + app.state.settle_done_after
    }

    fn closing_block_report(
        pane_id: &str,
        seq: u64,
        gates: Vec<crate::api::schema::ClosingBlockItem>,
    ) -> PaneReportAgentParams {
        let completion = if gates.is_empty() {
            crate::api::schema::ClosingCompletion::Complete
        } else {
            crate::api::schema::ClosingCompletion::Incomplete
        };
        PaneReportAgentParams {
            pane_id: pane_id.into(),
            source: "herdr:codex-closing-block".into(),
            agent: "codex".into(),
            state: crate::api::schema::PaneAgentState::Idle,
            v: Some(crate::api::schema::panes::CLOSING_BLOCK_VERSION),
            message: None,
            seq: Some(seq),
            wait: None,
            eta_s: None,
            reported_at: None,
            last_turn_at: None,
            agent_session_id: None,
            agent_session_path: None,
            gates: Some(gates),
            items: Some(Vec::new()),
            decisions: Some(Vec::new()),
            completion: Some(completion),
            external_wait: None,
            parse_status: Some(crate::api::schema::ClosingParseStatus::Ok),
            workers_unknown: Some(false),
            agents: Some(0),
            settle_ready: None,
        }
    }

    #[test]
    fn closing_report_projects_waiting_on_agents_until_a_zero_agent_close() {
        let (mut app, public_pane_id, pane_id, terminal_id) = quiet_settle_test_app();
        let mut waiting = closing_block_report(&public_pane_id, 1, Vec::new());
        waiting.state = crate::api::schema::PaneAgentState::Working;
        waiting.agents = Some(3);

        let response = app.handle_pane_report_agent("waiting-agents".into(), waiting);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let pane = app.pane_info(0, pane_id).expect("waiting pane info");
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert!(pane.waiting_on_agents);
        assert!(app.state.terminals[&terminal_id].waiting_on_agents());

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(None);
        let response = app.handle_pane_report_agent(
            "agents-finished".into(),
            closing_block_report(&public_pane_id, 2, Vec::new()),
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.raw_agent_state(),
            crate::detect::AgentState::Idle,
            "zero-agent close arbitration: {}",
            terminal.effective_state_arbitration()
        );
        let pane = app.pane_info(0, pane_id).expect("finished pane info");
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Done);
        assert!(!pane.waiting_on_agents);
        assert_eq!(
            app.state.terminals[&terminal_id].effective_active_subagents(),
            Some(0),
            "the adapter's zero declaration closes the report when no native scan exists"
        );
    }

    fn test_gate() -> crate::api::schema::ClosingBlockItem {
        test_attention_item("Gate")
    }

    fn test_attention_item(label: &str) -> crate::api::schema::ClosingBlockItem {
        crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: label.into(),
            text: "Choose the release path".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }
    }

    fn merged_work(url: &str) -> crate::work_index::Snapshot {
        crate::work_index::Snapshot {
            items: vec![crate::work_index::WorkItem {
                repo: "owner/repo".into(),
                pr_number: Some(23),
                pr_url: Some(url.into()),
                pr_title: None,
                pr_state: Some("merged".into()),
                draft: false,
                review_decision: None,
                created_at: None,
                updated_at: None,
                additions: 0,
                deletions: 0,
                author: None,
                assignees: Vec::new(),
                labels: Vec::new(),
                check_state: Default::default(),
                audience: Default::default(),
                cached_pr_detail: None,
                ticket_ids: Vec::new(),
                ticket_title: None,
                ticket_state: None,
                ticket_details: Vec::new(),
                branch: None,
                preview_urls: Vec::new(),
                panes: Vec::new(),
                source: Default::default(),
            }],
            conversations: Vec::new(),
            missive_users: Vec::new(),
            unavailable: None,
            observed_at: std::time::SystemTime::now(),
        }
    }

    fn assert_guard_blocks_overdue_inactivity_and_ripe_finished_work(guard: &str) {
        let activity_at = std::time::Instant::now();
        let now = activity_at + std::time::Duration::from_secs(120);
        let url = "https://github.com/owner/repo/pull/23";
        let work = merged_work(url);
        let (mut app, public_pane_id, pane_id, terminal_id) = quiet_settle_test_app();
        app.state.auto_settle_done = false;
        app.state.auto_settle_finished = true;
        app.state.auto_settle_inactive = false;
        app.state.settle_after = std::time::Duration::from_secs(60);
        app.state.settle_finished_after = std::time::Duration::from_secs(30);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .restore_work_context(crate::work_context::PaneWorkContext {
                pr_urls: vec![url.into()],
                ..Default::default()
            })
            .expect("valid work context");
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .activity
            .set_last_at(activity_at);
        let armed_at = now - app.state.settle_finished_after;
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), armed_at, 1_725_000_200),
            0
        );

        match guard {
            "closing gate" => {
                let mut report = closing_block_report(&public_pane_id, 1, vec![test_gate()]);
                report.state = crate::api::schema::PaneAgentState::Working;
                let response = app.handle_pane_report_agent("working-gate".into(), report);
                let _: SuccessResponse = serde_json::from_str(&response).unwrap();
            }
            "usage limit" => {
                app.handle_internal_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Codex),
                    state: AgentState::Blocked,
                    visible_blocker: false,
                    visible_working: false,
                    usage_limited: true,
                    process_exited: false,
                    observed_at: now,
                });
            }
            _ => unreachable!(),
        }
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .activity
            .set_last_at(activity_at);
        app.state.auto_settle_inactive = true;
        assert!(
            app.state.workspaces[0].tabs[0].panes[&pane_id]
                .activity
                .inactive_for(now)
                >= app.state.settle_after
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].panes[&pane_id].finished_since,
            Some(armed_at)
        );
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), now, 1_725_000_201),
            0,
            "{guard} must outrank both ripe settle triggers"
        );
        assert!(!app.state.pane_is_settled(0, pane_id));

        match guard {
            "closing gate" => {
                app.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                let response = app.handle_pane_report_agent(
                    "clear-working-gate".into(),
                    closing_block_report(&public_pane_id, 2, Vec::new()),
                );
                let _: SuccessResponse = serde_json::from_str(&response).unwrap();
            }
            "usage limit" => {
                app.handle_internal_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Codex),
                    state: AgentState::Idle,
                    visible_blocker: false,
                    visible_working: false,
                    usage_limited: false,
                    process_exited: false,
                    observed_at: now,
                });
            }
            _ => unreachable!(),
        }
        app.state.auto_settle_inactive = false;
        let cleared_at = std::time::Instant::now();
        assert_eq!(
            app.state
                .refresh_settled_panes_at(Some(&work), cleared_at, 1_725_000_202),
            0,
            "clearing {guard} must arm a fresh finished-work window"
        );
        assert_eq!(
            app.state.refresh_settled_panes_at(
                Some(&work),
                cleared_at + app.state.settle_finished_after,
                1_725_000_203,
            ),
            1
        );
        assert!(app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn working_closing_gate_blocks_overdue_inactivity_and_ripe_finished_work() {
        assert_guard_blocks_overdue_inactivity_and_ripe_finished_work("closing gate");
    }

    #[test]
    fn usage_limit_blocks_overdue_inactivity_and_ripe_finished_work() {
        assert_guard_blocks_overdue_inactivity_and_ripe_finished_work("usage limit");
    }

    fn assert_blocker_clear_starts_quiet_window(blocker: &str) {
        let (mut app, public_pane_id, pane_id, terminal_id) = quiet_settle_test_app();
        match blocker {
            "closing gate" => {
                let response = app.handle_pane_report_agent(
                    "gate".into(),
                    closing_block_report(&public_pane_id, 1, vec![test_gate()]),
                );
                let _: SuccessResponse = serde_json::from_str(&response).unwrap();
            }
            "usage limit" => {
                app.handle_internal_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Codex),
                    state: AgentState::Idle,
                    visible_blocker: false,
                    visible_working: false,
                    usage_limited: true,
                    process_exited: false,
                    observed_at: std::time::Instant::now(),
                });
            }
            _ => unreachable!(),
        }
        let ready_at = quiet_settle_ready_at(&app, pane_id);
        assert_eq!(
            app.state
                .refresh_settled_panes_at(None, ready_at, 1_725_000_000),
            0,
            "{blocker} must block settlement"
        );

        match blocker {
            "closing gate" => {
                app.retire_blocked_hook_authority_for_pane(pane_id, std::time::Instant::now());
                let response = app.handle_pane_report_agent(
                    "clear-gate".into(),
                    closing_block_report(&public_pane_id, 2, Vec::new()),
                );
                let _: SuccessResponse = serde_json::from_str(&response).unwrap();
            }
            "usage limit" => {
                app.handle_internal_event(crate::events::AppEvent::StateChanged {
                    pane_id,
                    agent: Some(Agent::Codex),
                    state: AgentState::Idle,
                    visible_blocker: false,
                    visible_working: false,
                    usage_limited: false,
                    process_exited: false,
                    observed_at: std::time::Instant::now(),
                });
            }
            _ => unreachable!(),
        }
        assert!(!app.state.terminals[&terminal_id].usage_limited);
        let cleared_at = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .activity
            .last_at();
        assert_eq!(
            app.state
                .refresh_settled_panes_at(None, cleared_at, 1_725_000_001),
            0,
            "clearing {blocker} must start, not consume, the quiet window"
        );
        assert_eq!(
            app.state.refresh_settled_panes_at(
                None,
                cleared_at + app.state.settle_done_after - std::time::Duration::from_nanos(1),
                1_725_001_799,
            ),
            0
        );
        // Clearing a gate reports fresh output, which stays unread until the human sees it.
        let settle_at = cleared_at + app.state.settle_done_after;
        if !app.state.workspaces[0].tabs[0].panes[&pane_id].seen {
            assert_eq!(
                app.state
                    .refresh_settled_panes_at(None, settle_at, 1_725_001_800),
                0,
                "unread result must not settle"
            );
            app.state.workspaces[0].tabs[0]
                .panes
                .get_mut(&pane_id)
                .unwrap()
                .seen = true;
        }
        assert_eq!(
            app.state.refresh_settled_panes_at(
                None,
                settle_at + std::time::Duration::from_nanos(1),
                1_725_001_801,
            ),
            1
        );
    }

    #[test]
    fn closing_gate_clear_starts_the_quiet_settle_window() {
        assert_blocker_clear_starts_quiet_window("closing gate");
    }

    #[test]
    fn usage_limit_clear_starts_the_quiet_settle_window() {
        assert_blocker_clear_starts_quiet_window("usage limit");
    }

    #[test]
    fn settle_guards_follow_focus_pin_and_agent_report_paths() {
        let (mut focused, public_pane_id, pane_id, _) = quiet_settle_test_app();
        let response = focused.handle_pane_focus(
            "focus".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ready_at = quiet_settle_ready_at(&focused, pane_id);
        assert_eq!(
            focused
                .state
                .refresh_settled_panes_at(None, ready_at, 1_725_000_100),
            0,
            "focused pane settled"
        );
        assert!(!focused.state.pane_is_settled(0, pane_id));

        let (mut pinned, _, pane_id, _) = quiet_settle_test_app();
        let tab_id = pinned.public_tab_id(0, 0).unwrap();
        let response = pinned.handle_tab_pin(
            "pin".into(),
            crate::api::schema::TabPinParams {
                tab_id,
                mode: crate::api::schema::TabPinMode::Pin,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ready_at = quiet_settle_ready_at(&pinned, pane_id);
        assert_eq!(
            pinned
                .state
                .refresh_settled_panes_at(None, ready_at, 1_725_000_101),
            0,
            "pinned tab settled"
        );
        assert!(!pinned.state.pane_is_settled(0, pane_id));

        let (mut blocked, public_pane_id, pane_id, terminal_id) = quiet_settle_test_app();
        blocked
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(None, AgentState::Unknown);
        let response = blocked.handle_pane_report_agent(
            "block".into(),
            PaneReportAgentParams {
                pane_id: public_pane_id,
                source: "custom:review".into(),
                agent: "review".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: None,
                message: None,
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: None,
                items: None,
                decisions: None,
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ready_at = quiet_settle_ready_at(&blocked, pane_id);
        assert_eq!(
            blocked
                .state
                .refresh_settled_panes_at(None, ready_at, 1_725_000_102),
            0,
            "blocked report settled"
        );
        assert!(!blocked.state.pane_is_settled(0, pane_id));

        let (mut gated, public_pane_id, pane_id, _) = quiet_settle_test_app();
        let response = gated.handle_pane_report_agent(
            "gate".into(),
            closing_block_report(&public_pane_id, 1, vec![test_gate()]),
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ready_at = quiet_settle_ready_at(&gated, pane_id);
        assert_eq!(
            gated
                .state
                .refresh_settled_panes_at(None, ready_at, 1_725_000_103),
            0,
            "closing-block report settled"
        );
        assert!(!gated.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn pane_settle_api_round_trip_updates_json_and_emits_transition_events() {
        let (mut app, pane_id) = app_with_test_workspace();
        let settle_response = app.handle_api_request(crate::api::schema::Request {
            id: "settle".into(),
            method: crate::api::schema::Method::PaneSettle(PaneTarget {
                pane_id: pane_id.clone(),
            }),
        });
        let settle: SuccessResponse = serde_json::from_str(&settle_response).expect("settle JSON");
        let ResponseResult::PaneInfo { pane: settled } = settle.result else {
            panic!("expected pane info");
        };
        let settled_at = settled.settled_at.expect("settled_at");
        let settle_events = app.event_hub.events_after(0);
        assert!(settle_events.iter().any(|(_, event)| {
            matches!(
                &event.data,
                EventData::PaneSettled {
                    pane_id: event_pane_id,
                    settled_at: event_settled_at,
                    ..
                } if event.event == EventKind::PaneSettled
                    && event_pane_id == &pane_id
                    && *event_settled_at == settled_at
            )
        }));
        let settle_seq = settle_events.last().map_or(0, |(seq, _)| *seq);

        let unsettle_response = app.handle_api_request(crate::api::schema::Request {
            id: "unsettle".into(),
            method: crate::api::schema::Method::PaneUnsettle(PaneTarget {
                pane_id: pane_id.clone(),
            }),
        });
        let unsettle: SuccessResponse =
            serde_json::from_str(&unsettle_response).expect("unsettle JSON");
        let ResponseResult::PaneInfo { pane: unsettled } = unsettle.result else {
            panic!("expected pane info");
        };
        assert_eq!(
            serde_json::to_value(unsettled).expect("pane JSON")["settled_at"],
            serde_json::Value::Null
        );
        assert!(app
            .event_hub
            .events_after(settle_seq)
            .iter()
            .any(|(_, event)| {
                matches!(
                    &event.data,
                    EventData::PaneUnsettled {
                        pane_id: event_pane_id,
                        ..
                    } if event.event == EventKind::PaneUnsettled
                        && event_pane_id == &pane_id
                )
            }));
        assert_eq!(pane_updated_events(&app), 2);
    }

    #[test]
    fn snooze_api_accepts_duration_or_deadline_and_rejects_invalid_time_inputs() {
        let (mut app, public_id, pane_id, _) = quiet_settle_test_app();
        let now = 1_725_000_000;
        for params in [
            serde_json::json!({"pane_id": public_id}),
            serde_json::json!({"pane_id": public_id, "duration_s": null}),
            serde_json::json!({
                "pane_id": public_id,
                "duration_s": 60,
                "snoozed_until": null
            }),
            serde_json::json!({
                "pane_id": public_id,
                "duration_s": 60,
                "snoozed_until": now + 60
            }),
        ] {
            let request = serde_json::json!({
                "id": "invalid-wire-snooze",
                "method": "pane.snooze",
                "params": params,
            });
            assert!(
                serde_json::from_value::<crate::api::schema::Request>(request).is_err(),
                "invalid wire request was accepted"
            );
        }
        let invalid = [
            (None, None, "invalid_snooze_request"),
            (Some(60), Some(now + 60), "invalid_snooze_request"),
            (Some(0), None, "invalid_snooze_duration"),
            (
                Some(crate::app::settled::MAX_SNOOZE_SECONDS + 1),
                None,
                "invalid_snooze_duration",
            ),
            (None, Some(now), "invalid_snooze_deadline"),
            (
                None,
                Some(now + crate::app::settled::MAX_SNOOZE_SECONDS + 1),
                "invalid_snooze_deadline",
            ),
        ];
        for (duration_s, snoozed_until, expected_code) in invalid {
            let response = app.handle_pane_snooze_at(
                "bad".into(),
                PaneSnoozeParams {
                    pane_id: public_id.clone(),
                    duration_s,
                    snoozed_until,
                },
                now,
            );
            let error: crate::api::schema::ErrorResponse =
                serde_json::from_str(&response).expect("invalid snooze error");
            assert_eq!(error.error.code, expected_code);
            assert!(app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .snoozed_until()
                .is_none());
        }

        let deadline = now + 3600;
        let response = app.handle_pane_snooze_at(
            "duration".into(),
            PaneSnoozeParams {
                pane_id: public_id.clone(),
                duration_s: Some(3600),
                snoozed_until: None,
            },
            now,
        );
        let success: SuccessResponse =
            serde_json::from_str(&response).expect("duration snooze response");
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("pane info");
        };
        assert_eq!(pane.snoozed_until, Some(deadline));
        app.handle_pane_unsnooze(
            "reset".into(),
            PaneTarget {
                pane_id: public_id.clone(),
            },
        );

        let response = app.handle_pane_snooze_at(
            "deadline".into(),
            PaneSnoozeParams {
                pane_id: public_id,
                duration_s: None,
                snoozed_until: Some(deadline),
            },
            now,
        );
        let success: SuccessResponse =
            serde_json::from_str(&response).expect("deadline snooze response");
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("pane info");
        };
        assert_eq!(pane.snoozed_until, Some(deadline));
    }

    #[test]
    fn snooze_and_settle_refuse_both_conflict_directions_without_replacing_state() {
        let (mut app, public_id, pane_id, _) = quiet_settle_test_app();
        let now = 1_725_000_000;
        assert!(app.state.settle_pane_at(0, pane_id, now));
        let response = app.handle_pane_snooze_at(
            "snooze-settled".into(),
            PaneSnoozeParams {
                pane_id: public_id.clone(),
                duration_s: Some(60),
                snoozed_until: None,
            },
            now,
        );
        let error: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("settled error");
        assert_eq!(error.error.code, "pane_settled");
        assert!(app.state.pane_is_settled(0, pane_id));
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .snoozed_until(),
            None
        );
        assert!(!app.state.snooze_pane_at(0, pane_id, now + 60));

        assert!(app
            .state
            .note_human_pane_activity_at(pane_id, std::time::Instant::now()));
        let response = app.handle_pane_snooze_at(
            "snooze".into(),
            PaneSnoozeParams {
                pane_id: public_id.clone(),
                duration_s: Some(60),
                snoozed_until: None,
            },
            now,
        );
        let _: SuccessResponse = serde_json::from_str(&response).expect("snooze response");
        let response = app.handle_pane_settlement(
            "settle".into(),
            PaneTarget {
                pane_id: public_id.clone(),
            },
            true,
        );
        let error: crate::api::schema::ErrorResponse =
            serde_json::from_str(&response).expect("settle error");
        assert_eq!(error.error.code, "pane_snoozed");
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .snoozed_until(),
            Some(now + 60)
        );
        assert!(!app.state.pane_is_settled(0, pane_id));
    }

    #[test]
    fn pane_and_agent_reads_expose_snooze_and_explicit_unsnooze_emits_event() {
        let (mut app, public_id, _, _) = quiet_settle_test_app();
        let now = 1_725_000_000;
        let deadline = now + 3600;
        let response = app.handle_pane_snooze_at(
            "snooze".into(),
            PaneSnoozeParams {
                pane_id: public_id.clone(),
                duration_s: None,
                snoozed_until: Some(deadline),
            },
            now,
        );
        let _: SuccessResponse = serde_json::from_str(&response).expect("snooze response");

        let pane_response = app.handle_api_request(crate::api::schema::Request {
            id: "pane".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget {
                pane_id: public_id.clone(),
            }),
        });
        let pane_response: SuccessResponse =
            serde_json::from_str(&pane_response).expect("pane.get response");
        let ResponseResult::PaneInfo { pane } = pane_response.result else {
            panic!("pane info");
        };
        assert_eq!(pane.snoozed_until, Some(deadline));

        let agent_response = app.handle_api_request(crate::api::schema::Request {
            id: "agents".into(),
            method: crate::api::schema::Method::AgentList(Default::default()),
        });
        let agent_response: SuccessResponse =
            serde_json::from_str(&agent_response).expect("agent.list response");
        let ResponseResult::AgentList { agents } = agent_response.result else {
            panic!("agent list");
        };
        assert_eq!(agents.len(), 1);
        assert_eq!(agents[0].snoozed_until, Some(deadline));

        let event_seq = app
            .event_hub
            .events_after(0)
            .last()
            .map_or(0, |(seq, _)| *seq);
        let response =
            app.handle_pane_unsnooze("unsnooze".into(), PaneTarget { pane_id: public_id });
        let success: SuccessResponse = serde_json::from_str(&response).expect("unsnooze response");
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("pane info");
        };
        assert_eq!(pane.snoozed_until, None);
        assert!(app
            .event_hub
            .events_after(event_seq)
            .iter()
            .any(|(_, event)| matches!(
                event.data,
                EventData::PaneUnsnoozed {
                    reason: crate::api::schema::PaneUnsnoozeReason::Explicit,
                    ..
                }
            )));
    }

    #[test]
    fn snooze_expiry_wakes_the_scheduler_marks_unread_emits_and_requests_redraw() {
        let (mut app, _, pane_id, _) = quiet_settle_test_app();
        let now = 1_725_000_000;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .done_since = Some(std::time::Instant::now());
        let terminal_id = app.state.workspaces[0]
            .pane_state(pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert!(crate::app::pane_lifecycle::pane_is_done(
            app.state.workspaces[0].pane_state(pane_id).unwrap(),
            app.state.terminals.get(&terminal_id).unwrap()
        ));
        assert!(app.state.snooze_pane_at(0, pane_id, now + 60));
        assert!(app.flush_pane_snooze_events());
        let before_expiry_seq = app
            .event_hub
            .events_after(0)
            .last()
            .map_or(0, |(seq, _)| *seq);
        assert!(!crate::app::pane_lifecycle::pane_is_done(
            app.state.workspaces[0].pane_state(pane_id).unwrap(),
            app.state.terminals.get(&terminal_id).unwrap()
        ));
        let clock = std::time::Instant::now();
        assert_eq!(
            app.state.next_snooze_deadline_at(clock, now),
            Some(clock + std::time::Duration::from_secs(60))
        );
        assert!(!app.refresh_pane_settlement_at_with_wall_clock(clock, now + 59));
        assert_eq!(
            app.state.workspaces[0]
                .pane_state(pane_id)
                .unwrap()
                .snoozed_until(),
            Some(now + 60)
        );
        app.state.workspaces[0]
            .pane_state_mut(pane_id)
            .unwrap()
            .seen = true;
        let expiry = clock + std::time::Duration::from_secs(1);
        assert!(app.refresh_pane_settlement_at_with_wall_clock(expiry, now + 60));
        let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
        assert!(pane.snoozed_until().is_none());
        assert!(!pane.seen);
        assert_eq!(pane.done_since, Some(expiry));
        assert!(app
            .event_hub
            .events_after(before_expiry_seq)
            .iter()
            .any(|(_, event)| matches!(
                event.data,
                EventData::PaneUnsnoozed {
                    reason: crate::api::schema::PaneUnsnoozeReason::Expired,
                    ..
                }
            )));
    }

    #[test]
    fn gate_and_answer_attention_end_snooze_and_emit_immediately() {
        for label in ["Gate", "Answer"] {
            let (mut app, public_id, pane_id, terminal_id) = quiet_settle_test_app();
            assert!(app.state.snooze_pane_at(0, pane_id, 1_725_000_120));
            app.flush_pane_snooze_events();
            let before_seq = app
                .event_hub
                .events_after(0)
                .last()
                .map_or(0, |(seq, _)| *seq);
            let mut report = closing_block_report(&public_id, 1, Vec::new());
            if label == "Gate" {
                report.gates = Some(vec![test_attention_item(label)]);
            } else {
                report.items = Some(vec![test_attention_item(label)]);
            }

            let response = app.handle_pane_report_agent(label.into(), report);
            let _: SuccessResponse = serde_json::from_str(&response).expect("attention report");
            let pane = app.state.workspaces[0].pane_state(pane_id).unwrap();
            assert_eq!(pane.snoozed_until(), None, "{label}");
            assert_eq!(
                pane.agent_projection(&app.state.terminals[&terminal_id])
                    .attention_tier,
                crate::terminal::state::AttentionTier::Blocked,
                "{label}"
            );
            assert!(
                app.event_hub
                    .events_after(before_seq)
                    .iter()
                    .any(|(_, event)| matches!(
                        event.data,
                        EventData::PaneUnsnoozed {
                            reason: crate::api::schema::PaneUnsnoozeReason::Attention,
                            ..
                        }
                    )),
                "{label}"
            );
        }
    }

    #[test]
    fn pane_unsettle_api_explicitly_clears_settled_at() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        assert!(app.state.settle_pane_at(0, internal_pane_id, 1_725_000_000));

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "unsettle".into(),
            method: crate::api::schema::Method::PaneUnsettle(PaneTarget {
                pane_id: pane_id.clone(),
            }),
        });
        let response: SuccessResponse = serde_json::from_str(&response).expect("unsettle response");
        let ResponseResult::PaneInfo { pane } = response.result else {
            panic!("expected pane info");
        };

        assert!(pane.settled_at.is_none());
        assert!(!app.state.pane_is_settled(0, internal_pane_id));
    }

    #[test]
    fn ac4_work_context_patch_is_atomic_exposed_and_emits_once_per_mutation() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&internal_pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(Agent::Codex);

        let response = app.handle_pane_work_context_set(
            "set".into(),
            PaneWorkContextSetParams {
                pane_id: pane_id.clone(),
                patch: crate::work_context::PaneWorkContextPatch {
                    repo: None,
                    ticket_ids: Some(vec!["mat-7".into(), "SCA-2".into()]),
                    pr_urls: Some(vec![
                        "https://github.com/o/r/pull/08".into(),
                        "https://github.com/o/r/pull/09".into(),
                    ]),
                    missive_urls: Some(vec![
                        "https://MAIL.missiveapp.com/#inbox/conversations/sample-1".into(),
                    ]),
                    branch: Some("feat/context".into()),
                    work_title: Some("Context model".into()),
                    role: Some(crate::work_context::PaneWorkRole::Ship),
                    active_owner: Some(true),
                    clear_fields: Vec::new(),
                },
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert_eq!(pane.work_context.ticket_ids, vec!["SCA-2"]);
        assert_eq!(
            pane.work_context.pr_urls,
            vec!["https://github.com/o/r/pull/9"]
        );
        assert_eq!(
            pane.work_context.missive_urls,
            vec!["https://mail.missiveapp.com/#inbox/conversations/sample-1"]
        );
        assert_eq!(pane.work_context.branch.as_deref(), Some("feat/context"));
        assert_eq!(
            pane.work_context.work_title.as_deref(),
            Some("Context model")
        );
        assert_eq!(
            pane.work_context.role,
            Some(crate::work_context::PaneWorkRole::Ship)
        );
        assert!(pane.work_context.active_owner);
        assert_eq!(
            app.state.terminals[&terminal_id]
                .work_context
                .snapshot_tiers()
                .manual,
            pane.work_context
        );
        assert_eq!(app.collect_agent_infos()[0].work_context, pane.work_context);
        assert_eq!(pane_updated_events(&app), 1);
        let (_, event) = app
            .event_hub
            .events_after(0)
            .into_iter()
            .find(|(_, event)| event.event == EventKind::PaneUpdated)
            .expect("pane.updated event");
        let crate::api::schema::EventData::PaneUpdated { pane: updated_pane } = event.data else {
            panic!("expected pane.updated payload");
        };
        assert_eq!(updated_pane.pane_id, pane_id);
        assert_eq!(updated_pane.work_context, pane.work_context);

        let no_op = app.handle_pane_work_context_set(
            "noop".into(),
            PaneWorkContextSetParams {
                pane_id: pane_id.clone(),
                patch: crate::work_context::PaneWorkContextPatch {
                    repo: None,
                    ticket_ids: Some(vec!["MAT-7".into(), "SCA-2".into()]),
                    pr_urls: Some(vec!["https://github.com/o/r/pull/9".into()]),
                    missive_urls: Some(vec![
                        "https://mail.missiveapp.com/#inbox/conversations/sample-1".into(),
                    ]),
                    branch: Some("feat/context".into()),
                    work_title: Some("Context model".into()),
                    role: Some(crate::work_context::PaneWorkRole::Ship),
                    active_owner: Some(true),
                    clear_fields: Vec::new(),
                },
            },
        );
        assert!(serde_json::from_str::<SuccessResponse>(&no_op).is_ok());
        assert_eq!(pane_updated_events(&app), 1);

        let before = app.pane_info(0, internal_pane_id).unwrap();
        let invalid = app.handle_pane_work_context_set(
            "invalid".into(),
            PaneWorkContextSetParams {
                pane_id,
                patch: crate::work_context::PaneWorkContextPatch {
                    repo: None,
                    ticket_ids: Some(vec!["SCA-99".into()]),
                    pr_urls: Some(vec!["https://evil.test/o/r/pull/1".into()]),
                    ..Default::default()
                },
            },
        );
        assert_eq!(metadata_error_code(&invalid), "invalid_work_context");
        assert_eq!(app.pane_info(0, internal_pane_id).unwrap(), before);
        assert_eq!(pane_updated_events(&app), 1);
    }

    #[test]
    fn live_work_context_claim_is_demoted_when_another_pane_owns_the_pr() {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("owner"), Workspace::test_new("review")];
        app.state.ensure_test_terminals();
        let panes = app
            .state
            .workspaces
            .iter()
            .enumerate()
            .map(|(ws_idx, workspace)| {
                let pane_id = workspace.tabs[0].root_pane;
                (
                    app.public_pane_id(ws_idx, pane_id).unwrap(),
                    workspace.terminal_id(pane_id).cloned().unwrap(),
                )
            })
            .collect::<Vec<_>>();
        let claim = |pane_id: String, role| PaneWorkContextSetParams {
            pane_id,
            patch: crate::work_context::PaneWorkContextPatch {
                pr_urls: Some(vec!["https://github.com/o/r/pull/42".into()]),
                role: Some(role),
                active_owner: Some(true),
                ..Default::default()
            },
        };

        let first = app.handle_pane_work_context_set(
            "owner".into(),
            claim(panes[0].0.clone(), crate::work_context::PaneWorkRole::Ship),
        );
        assert!(serde_json::from_str::<SuccessResponse>(&first).is_ok());
        let second = app.handle_pane_work_context_set(
            "review".into(),
            claim(
                panes[1].0.clone(),
                crate::work_context::PaneWorkRole::Review,
            ),
        );
        let success: SuccessResponse = serde_json::from_str(&second).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };

        let owner = app.state.terminals[&panes[0].1].effective_work_context();
        assert_eq!(owner.role, Some(crate::work_context::PaneWorkRole::Ship));
        assert!(owner.active_owner);
        assert_eq!(
            pane.work_context.role,
            Some(crate::work_context::PaneWorkRole::Review)
        );
        assert!(!pane.work_context.active_owner);
        assert_eq!(
            app.state.terminals[&panes[1].1].effective_work_context(),
            &pane.work_context
        );
    }

    fn pane_updated_events(app: &App) -> usize {
        app.event_hub
            .events_after(0)
            .iter()
            .filter(|(_, event)| event.event == EventKind::PaneUpdated)
            .count()
    }

    fn app_with_send_key_runtime(
        capacity: usize,
    ) -> (App, String, tokio::sync::mpsc::Receiver<bytes::Bytes>) {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, rx) =
            crate::terminal::TerminalRuntime::test_with_channel_capacity(80, 24, capacity);
        app.state.insert_test_runtime(pane_id, runtime);
        (app, public_pane_id, rx)
    }

    fn app_with_scrollback_runtime() -> (App, String, PaneId) {
        let (mut app, public_pane_id) = app_with_test_workspace();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let lines = (0..20)
            .map(|line| format!("line {line:02}\n"))
            .collect::<String>();
        let runtime = crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
            20,
            5,
            1000,
            lines.as_bytes(),
        );
        app.state.insert_test_runtime(pane_id, runtime);
        (app, public_pane_id, pane_id)
    }

    fn conditional_send_params(app: &mut App, pane_id: &str) -> PaneSendTextIfParams {
        let terminal_id =
            bind_test_agent_session(app, pane_id, "herdr:codex", "codex", "session-1");
        let _ = app.sync_terminal_titles();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(pane_id).unwrap();
        let observation = app
            .lookup_runtime_sender(workspace_idx, internal_pane_id)
            .unwrap()
            .input_observation()
            .expect("coherent input observation");
        PaneSendTextIfParams {
            pane_id: pane_id.into(),
            text: "answer".into(),
            workspace_id: app.public_workspace_id(workspace_idx),
            terminal_id: terminal_id.to_string(),
            agent_ref: crate::api::schema::AgentRef::new(
                app.state.agent_host_name.clone(),
                pane_id,
            )
            .unwrap(),
            agent_session: crate::api::schema::AgentSessionInfo {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "session-1".into(),
            },
            condition: crate::api::schema::PaneSendTextCondition::DetectionSnapshotUnchanged,
            observation_token: observation.token,
        }
    }

    fn metadata_params(pane_id: String) -> PaneReportMetadataParams {
        PaneReportMetadataParams {
            pane_id,
            source: "user:metadata.test-1".into(),
            agent: None,
            applies_to_source: None,
            agent_session_id: None,
            title: Some("activity".into()),
            work_context: None,
            work_title_long_brief: false,
            display_agent: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::new(),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            seq: None,
            ttl_ms: None,
        }
    }

    #[cfg(unix)]
    fn closing_block_adapter_requests(
        pane_id: &str,
        session_id: &str,
        items: serde_json::Value,
    ) -> Vec<serde_json::Value> {
        let asset_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/integration/assets/closing-block");
        let input = serde_json::to_string(&items).expect("serialize adapter input");
        let output = std::process::Command::new("python3")
            .current_dir(asset_dir)
            .args([
                "-c",
                "import json, sys; import herdr_status; calls = []; mirror_calls = []; herdr_status.write_mirror = lambda pane_id, payload: mirror_calls.append((pane_id, payload)); herdr_status._rpc = lambda _sock, _source, method, params: calls.append({'method': method, 'params': params}); items = json.loads(sys.argv[1]); action_labels = {'gate', 'answer', 'verify'}; has_action = any(str(item.get('label', '')).strip().lower() in action_labels for item in items); herdr_status.report(agent='claude', blocking=0, agents=0, items=items, completion='incomplete' if has_action else 'complete', parse_status='ok', pane_id=sys.argv[2], sock_path='test.sock', session_id=sys.argv[3]); assert len(mirror_calls) == 1; print(json.dumps(calls))",
                &input,
                pane_id,
                session_id,
            ])
            .output()
            .expect("run closing-block adapter");
        assert!(
            output.status.success(),
            "adapter failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("parse closing-block adapter requests")
    }

    #[cfg(unix)]
    fn closing_block_text_adapter_requests(
        pane_id: &str,
        session_id: &str,
        text: &str,
    ) -> Vec<serde_json::Value> {
        let asset_dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR"))
            .join("src/integration/assets/closing-block");
        let output = std::process::Command::new("python3")
            .current_dir(asset_dir)
            .args([
                "-c",
                "import json, sys; import closing_block, herdr_status; calls = []; mirror_calls = []; herdr_status.write_mirror = lambda pane_id, payload: mirror_calls.append((pane_id, payload)); herdr_status._rpc = lambda _sock, _source, method, params: calls.append({'method': method, 'params': params}); block = closing_block.parse(sys.argv[1]); herdr_status.report(agent='claude', blocking=block.blocking, agents=block.agents_running, gates=block.wire_gates(), items=block.wire_items(), decisions=block.wire_decisions(), agent_names=block.agents, contract=block.contract, contract_met=block.contract_met, completion=block.completion, external_wait=block.external_wait, parse_status=block.parse_status, workers_unknown=block.workers_unknown, pane_id=sys.argv[2], sock_path='test.sock', session_id=sys.argv[3]); assert len(mirror_calls) == 1; print(json.dumps(calls))",
                text,
                pane_id,
                session_id,
            ])
            .output()
            .expect("run closing-block parser and adapter");
        assert!(
            output.status.success(),
            "adapter failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        serde_json::from_slice(&output.stdout).expect("parse closing-block adapter requests")
    }

    #[cfg(unix)]
    fn apply_closing_block_adapter_requests(
        app: &mut App,
        prefix: &str,
        requests: Vec<serde_json::Value>,
    ) {
        assert_eq!(requests.len(), 3, "adapter reports the complete RPC chain");
        for (index, request) in requests.into_iter().enumerate() {
            let request: crate::api::schema::Request = serde_json::from_value(serde_json::json!({
                "id": format!("{prefix}-{index}"),
                "method": request["method"],
                "params": request["params"],
            }))
            .expect("adapter request matches the runtime schema");
            let response = app.handle_api_request(request);
            let _: SuccessResponse = serde_json::from_str(&response)
                .unwrap_or_else(|_| panic!("adapter request {index} failed: {response}"));
        }
    }

    fn metadata_error_code(response: &str) -> String {
        let response: ErrorResponse = serde_json::from_str(response).unwrap();
        response.error.code
    }

    fn bind_test_agent_session(
        app: &mut App,
        pane_id: &str,
        source: &str,
        agent: &str,
        session_id: &str,
    ) -> crate::terminal::TerminalId {
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: source.into(),
                agent: agent.into(),
                session_ref: crate::agent_resume::AgentSessionRef::id(session_id).unwrap(),
            });
        terminal_id
    }

    fn bind_test_live_agent_session(
        app: &mut App,
        pane_id: &str,
        source: &str,
        agent_label: &str,
        session_id: &str,
    ) -> crate::terminal::TerminalId {
        bind_test_live_agent_authority(
            app,
            pane_id,
            source,
            agent_label,
            Some(
                crate::agent_resume::AgentSessionRef::id(session_id)
                    .expect("valid test session id"),
            ),
        )
    }

    fn bind_test_live_agent_authority(
        app: &mut App,
        pane_id: &str,
        source: &str,
        agent_label: &str,
        session_ref: Option<crate::agent_resume::AgentSessionRef>,
    ) -> crate::terminal::TerminalId {
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let agent = crate::detect::parse_agent_label(agent_label).expect("known test agent");
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(agent), AgentState::Working);
        let anchor = session_ref.clone().or_else(|| {
            crate::detect::full_lifecycle_hook_authority(source, agent_label).then(|| {
                crate::agent_resume::AgentSessionRef::id("fixture-anchor")
                    .expect("valid test session id")
            })
        });
        if let Some(anchor) = anchor {
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: source.into(),
                agent: agent_label.into(),
                session_ref: anchor,
            });
        }
        let mutation = terminal.set_hook_authority_with_session_ref(
            source.into(),
            agent_label.into(),
            AgentState::Working,
            None,
            session_ref.clone(),
            Some(1),
        );
        assert!(mutation.is_some(), "test hook authority must be live");
        if let Some(session_ref) = session_ref {
            terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: source.into(),
                agent: agent_label.into(),
                session_ref,
            });
        }
        terminal_id
    }

    fn guarded_work_title_params(
        pane_id: String,
        agent: &str,
        lifecycle_source: &str,
        session_id: &str,
        title: &str,
        seq: u64,
    ) -> PaneReportMetadataParams {
        let mut params = metadata_params(pane_id);
        params.source = crate::work_title::WORK_TITLE_SOURCE.into();
        params.agent = Some(agent.into());
        params.applies_to_source = Some(lifecycle_source.into());
        params.agent_session_id = Some(session_id.into());
        params.title = Some(title.into());
        params.work_title_long_brief = false;
        params.seq = Some(seq);
        params
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_control_navigation_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec![
                    "ctrl+h".into(),
                    "ctrl+j".into(),
                    "ctrl+k".into(),
                    "ctrl+l".into(),
                ],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x08]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0b]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0c]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_encodes_shift_tab_as_backtab() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["shift+tab".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"\x1b[Z"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_get_exposes_scroll_metrics() {
        let (mut app, public_pane_id, pane_id) = app_with_scrollback_runtime();
        let runtime = app
            .state
            .runtime_for_pane_in_workspace(&app.terminal_runtimes, 0, pane_id)
            .expect("runtime");
        runtime.scroll_up(3);

        let response = app.handle_pane_get(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        let scroll = pane.scroll.expect("scroll metrics");
        assert_eq!(scroll.offset_from_bottom, 3);
        assert!(scroll.max_offset_from_bottom >= scroll.offset_from_bottom);
        assert_eq!(scroll.viewport_rows, 5);
    }

    #[tokio::test]
    async fn api_pane_read_reports_when_older_rows_are_omitted() {
        let (mut app, public_pane_id, _pane_id) = app_with_scrollback_runtime();

        let response = app.handle_pane_read(
            "req".into(),
            PaneReadParams {
                pane_id: public_pane_id,
                source: crate::api::schema::ReadSource::Recent,
                lines: Some(2),
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
                intent: crate::api::schema::ReadIntent::Interactive,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneRead { read } = success.result else {
            panic!("expected pane read response");
        };
        assert!(read.text.contains("line 19"));
        assert!(read.truncated);
    }

    #[tokio::test]
    async fn api_detection_read_exposes_identity_and_a_coherent_observation() {
        let (mut app, pane_id, _rx) = app_with_send_key_runtime(1);
        let params = conditional_send_params(&mut app, &pane_id);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "read".into(),
            method: crate::api::schema::Method::PaneRead(PaneReadParams {
                pane_id,
                source: crate::api::schema::ReadSource::Detection,
                lines: None,
                format: crate::api::schema::ReadFormat::Text,
                strip_ansi: true,
                intent: crate::api::schema::ReadIntent::Passive,
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneRead { read } = success.result else {
            panic!("expected pane read response");
        };
        assert_eq!(read.terminal_id, params.terminal_id);
        assert_eq!(read.agent_ref.as_ref(), Some(&params.agent_ref));
        assert_eq!(read.agent_session.as_ref(), Some(&params.agent_session));
        assert!(read.input_observation.is_some());
    }

    #[tokio::test]
    async fn api_pane_send_keys_preserves_legacy_control_c_aliases() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(3);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["C-c".into(), "c-c".into(), "ctrl+c".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x03]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_accepts_literal_plus() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["+".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"+"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_keys_sends_shifted_punctuation_as_text_in_kitty_mode() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[>7u",
                1,
            );
        app.state.insert_test_runtime(internal_pane_id, runtime);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["shift+?".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"?"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_brackets_text_and_enter_atomically() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        app.lookup_runtime_sender(0, internal_pane_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b[?2004h");

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: "A != B".into(),
                keys: vec!["Enter".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(
            rx.try_recv().unwrap(),
            bytes::Bytes::from_static(b"\x1b[200~A != B\x1b[201~\r")
        );
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_conditional_send_matches_all_identity_and_enqueues_once() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);
        let params = conditional_send_params(&mut app, &pane_id);
        let (ws_idx, internal_pane_id) = app.parse_current_public_pane_id(&params.pane_id).unwrap();
        let terminal_id = app.state.workspaces[ws_idx]
            .terminal_id(internal_pane_id)
            .unwrap();
        assert_eq!(app.public_workspace_id(ws_idx), params.workspace_id);
        assert_eq!(terminal_id.as_str(), params.terminal_id);
        assert_eq!(
            crate::api::schema::AgentRef::new(
                app.state.agent_host_name.clone(),
                params.pane_id.clone()
            )
            .unwrap(),
            params.agent_ref
        );
        assert_eq!(
            crate::app::creation::terminal_agent_session_info(
                app.state.terminals.get(terminal_id).unwrap()
            )
            .as_ref(),
            Some(&params.agent_session)
        );
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "conditional".into(),
            method: crate::api::schema::Method::PaneSendTextIf(params),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            success.result,
            ResponseResult::PaneTextSend {
                outcome: crate::api::schema::PaneSendTextIfOutcome::Sent
            }
        );
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from_static(b"answer"));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_conditional_send_rejects_each_stale_identity_without_enqueuing() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(8);
        let params = conditional_send_params(&mut app, &pane_id);
        let mut mismatches = Vec::new();
        let mut changed = params.clone();
        changed.pane_id.push_str("-stale");
        mismatches.push(changed);
        let mut changed = params.clone();
        changed.workspace_id.push_str("-stale");
        mismatches.push(changed);
        let mut changed = params.clone();
        changed.terminal_id.push_str("-stale");
        mismatches.push(changed);
        let mut changed = params.clone();
        changed.agent_ref.agent.push_str("-stale");
        mismatches.push(changed);
        let mut changed = params.clone();
        changed.agent_session.value.push_str("-stale");
        mismatches.push(changed);
        let mut changed = params;
        changed.observation_token.push_str("-stale");
        mismatches.push(changed);

        for params in mismatches {
            let response = app.handle_pane_send_text_if("conditional".into(), params);
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(
                success.result,
                ResponseResult::PaneTextSend {
                    outcome: crate::api::schema::PaneSendTextIfOutcome::ConditionMismatch
                }
            );
        }
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_keys_accept_key_combo_chords() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: String::new(),
                keys: vec!["ctrl+j".into()],
            }),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(success.result, ResponseResult::Ok {});
        assert_eq!(rx.try_recv().unwrap(), bytes::Bytes::from(vec![0x0a]));
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pane_write_apis_retire_a_blocked_hook_after_forwarding_bytes() {
        let requests = [
            crate::api::schema::Method::PaneSendText(PaneSendTextParams {
                pane_id: String::new(),
                text: "x".into(),
            }),
            crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id: String::new(),
                keys: vec!["Enter".into()],
            }),
            crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id: String::new(),
                text: "x".into(),
                keys: Vec::new(),
            }),
        ];

        for (index, mut method) in requests.into_iter().enumerate() {
            let (mut app, pane_id, mut rx) = app_with_send_key_runtime(1);
            let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
            let terminal_id = app.state.workspaces[0]
                .terminal_id(internal_pane_id)
                .unwrap()
                .clone();
            let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
            terminal.set_detected_state(Some(Agent::Codex), AgentState::Idle);
            terminal.set_hook_authority(
                "herdr:codex-closing-block".into(),
                "codex".into(),
                AgentState::Blocked,
                None,
                Some(1),
            );
            assert_eq!(
                app.state.terminals[&terminal_id].raw_agent_state(),
                AgentState::Blocked
            );

            match &mut method {
                crate::api::schema::Method::PaneSendText(params) => {
                    params.pane_id = pane_id.clone()
                }
                crate::api::schema::Method::PaneSendKeys(params) => {
                    params.pane_id = pane_id.clone()
                }
                crate::api::schema::Method::PaneSendInput(params) => {
                    params.pane_id = pane_id.clone()
                }
                _ => unreachable!(),
            }
            let response = app.handle_api_request(crate::api::schema::Request {
                id: format!("req-{index}"),
                method,
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();

            assert_eq!(success.result, ResponseResult::Ok {});
            assert!(rx.try_recv().is_ok());
            assert_eq!(
                app.state.terminals[&terminal_id].raw_agent_state(),
                AgentState::Idle
            );
            assert!(!app.state.terminals[&terminal_id].full_lifecycle_hook_authority_active());
        }
    }

    #[tokio::test]
    async fn api_pane_send_keys_rejects_invalid_keys_before_writing() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(2);

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendKeys(PaneSendKeysParams {
                pane_id,
                keys: vec!["ctrl+h".into(), "not-a-key".into()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, "unsupported key not-a-key");
        assert!(rx.try_recv().is_err());
    }

    #[tokio::test]
    async fn api_pane_send_input_rejects_prefix_bindings_before_writing_text_or_keys() {
        let (mut app, pane_id, mut rx) = app_with_send_key_runtime(4);
        let raw_key = " prefix+h ".to_string();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneSendInput(PaneSendInputParams {
                pane_id,
                text: "hello".into(),
                keys: vec!["ctrl+h".into(), raw_key.clone()],
            }),
        });

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "invalid_key");
        assert_eq!(error.error.message, format!("unsupported key {raw_key}"));
        assert!(rx.try_recv().is_err());
    }

    fn app_with_linked_worktree() -> App {
        let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
        let mut app = App::new(
            &Config::default(),
            true,
            None,
            api_rx,
            crate::api::EventHub::default(),
        );
        app.state.workspaces = vec![Workspace::test_new("issue")];
        app.state.ensure_test_terminals();
        app.state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: "/repo/herdr".into(),
            checkout_path: "/repo/herdr-issue".into(),
            is_linked_worktree: true,
        });
        app
    }

    fn seed_terminal_states(app: &mut App) {
        for ws in &app.state.workspaces {
            for tab in &ws.tabs {
                for pane in tab.panes.values() {
                    app.state
                        .terminals
                        .entry(pane.attached_terminal_id.clone())
                        .or_insert_with(|| {
                            crate::terminal::TerminalState::new(
                                pane.attached_terminal_id.clone(),
                                std::path::PathBuf::from("/herdr-test"),
                            )
                        });
                }
            }
        }
    }

    #[test]
    fn api_pane_close_drops_agent_state_and_late_links_do_not_recreate_it() {
        let (mut app, public_pane_id) = app_with_test_workspace();
        app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        let (_, pane_id) = app.parse_pane_id(&public_pane_id).unwrap();
        app.state
            .agent_states
            .report(
                pane_id,
                crate::agent_state::AgentReportPayload {
                    goal: Some(Some("remove on close".into())),
                    ..crate::agent_state::AgentReportPayload::default()
                },
                std::time::SystemTime::now(),
            )
            .expect("valid report");

        let response = app.handle_pane_close(
            "close".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let after_close = app
            .state
            .agent_states
            .snapshot(pane_id, crate::api::schema::AgentStatus::Unknown);
        assert!(after_close.goal.is_none());

        app.state
            .handle_app_event(crate::events::AppEvent::AgentLinksDetected {
                pane_id,
                output_urls: vec!["https://late.example.test/stale".into()],
                osc8_urls: Vec::new(),
                observed_at: std::time::SystemTime::now(),
            });
        assert!(app
            .state
            .agent_states
            .snapshot(pane_id, crate::api::schema::AgentStatus::Unknown)
            .links
            .is_empty());
    }

    #[tokio::test]
    async fn api_pane_close_last_pane_closes_linked_worktree_workspace() {
        let mut app = app_with_linked_worktree();
        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();

        let response = app.handle_pane_close(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
        assert_eq!(app.state.request_remove_linked_worktree, None);
        assert!(app.state.workspaces.is_empty());
        for (_terminal_id, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn api_pane_close_removes_orphaned_companions_with_their_tab() {
        let (mut app, _) = app_with_test_workspace();
        let primary = app.state.workspaces[0].tabs[0].root_pane;
        let companion = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(primary);
        app.state.workspaces[0]
            .pane_state_mut(companion)
            .unwrap()
            .is_companion = true;
        app.state.ensure_test_terminals();
        let primary_terminal = app.state.terminal_id_for_pane(0, primary).unwrap();
        let companion_terminal = app.state.terminal_id_for_pane(0, companion).unwrap();
        let public_primary = app.public_pane_id(0, primary).unwrap();

        let response = app.handle_pane_close(
            "close".into(),
            PaneTarget {
                pane_id: public_primary,
            },
        );

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(app.state.workspaces.is_empty());
        assert!(!app.state.terminals.contains_key(&primary_terminal));
        assert!(!app.state.terminals.contains_key(&companion_terminal));
        for (_terminal_id, runtime) in app.terminal_runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[test]
    fn api_pane_current_prefers_caller_pane_id() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.ensure_test_terminals();
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams {
                caller_pane_id: Some(right_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, right_public);
        assert!(!pane.focused);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
        assert_ne!(pane.pane_id, root_public);
    }

    #[test]
    fn api_pane_current_falls_back_to_focused_pane() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams::default(),
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, root_public);
        assert!(pane.focused);
    }

    #[test]
    fn api_pane_current_dispatches_through_socket_request() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneCurrent(
                crate::api::schema::PaneCurrentParams::default(),
            ),
        });

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneCurrent { pane } = success.result else {
            panic!("expected pane current response");
        };
        assert_eq!(pane.pane_id, root_public);
    }

    #[test]
    fn api_pane_current_reports_invalid_caller_pane_id() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams {
                caller_pane_id: Some("missing".into()),
            },
        );

        assert_eq!(metadata_error_code(&response), "pane_not_found");
    }

    #[test]
    fn api_pane_current_reports_no_active_pane() {
        let mut app = app_with_linked_worktree();
        app.state.active = None;

        let response = app.handle_pane_current(
            "req".into(),
            crate::api::schema::PaneCurrentParams::default(),
        );

        assert_eq!(metadata_error_code(&response), "pane_not_found");
    }

    #[test]
    fn api_pane_swap_explicit_source_and_target_preserves_focus_and_returns_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(swap.reason, None);
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.focused_pane_id, swap.source_pane_id);
        assert_eq!(swap.layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
    }

    #[test]
    fn api_pane_swap_unfocused_source_updates_last_pane_history() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let focused = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let target = app.state.workspaces[0].test_split(ratatui::layout::Direction::Vertical);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(focused);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public),
                target_pane_id: Some(target_public),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(swap.changed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));

        app.state.last_pane();

        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(focused));
    }

    #[test]
    fn api_pane_swap_direction_no_neighbor_returns_unchanged_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                pane_id: Some(source_public.clone()),
                direction: Some(PaneDirection::Left),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NoNeighbor));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, None);
        assert_eq!(swap.layout.panes.len(), 1);
        assert!(app.event_hub.events_after(0).is_empty());
    }

    #[test]
    fn api_pane_swap_explicit_missing_target_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some("missing-pane".into()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some("missing-pane".into()));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_missing_source_returns_not_found_noop() {
        let mut app = app_with_linked_worktree();
        let target = app.state.workspaces[0].tabs[0].root_pane;
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some("missing-pane".into()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::NotFound));
        assert_eq!(swap.source_pane_id, "missing-pane");
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.panes.len(), 1);
    }

    #[test]
    fn api_pane_swap_explicit_cross_workspace_preserves_target_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_swap(
            "req".into(),
            PaneSwapParams {
                source_pane_id: Some(source_public.clone()),
                target_pane_id: Some(target_public.clone()),
                ..PaneSwapParams::default()
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneSwap { swap } = success.result else {
            panic!("expected pane swap response");
        };
        assert!(!swap.changed);
        assert_eq!(swap.reason, Some(PaneSwapReason::CrossTab));
        assert_eq!(swap.source_pane_id, source_public);
        assert_eq!(swap.target_pane_id, Some(target_public));
        assert_eq!(swap.layout.workspace_id, app.public_workspace_id(0));
    }

    #[test]
    fn api_pane_move_to_existing_tab_preserves_internal_pane_and_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab_public = app.public_tab_id(0, 0).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        app.state
            .terminals
            .get_mut(&source_terminal)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: None,
                ticket_ids: Some(vec!["MAT-42".into()]),
                work_title: Some("Move context".into()),
                ..Default::default()
            })
            .unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: Some(0.25),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.reason, None);
        assert_eq!(move_result.previous_pane_id, source_public);
        assert_eq!(move_result.previous_tab_id, source_tab_public);
        assert_eq!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert_eq!(move_result.pane.tab_id, target_tab_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(move_result.pane.work_context.ticket_ids, vec!["MAT-42"]);
        assert_eq!(
            move_result.pane.work_context.work_title.as_deref(),
            Some("Move context")
        );
        assert_eq!(move_result.closed_tab_id, Some(source_tab_public));
        assert_eq!(move_result.closed_workspace_id, None);
        assert_eq!(move_result.target_layout.panes.len(), 2);
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_to_existing_tab_preserves_four_way_placement() {
        for (split, moved_first) in [
            (SplitDirection::Left, true),
            (SplitDirection::Right, false),
            (SplitDirection::Up, true),
            (SplitDirection::Down, false),
        ] {
            let mut app = app_with_linked_worktree();
            let source = app.state.workspaces[0].tabs[0].root_pane;
            let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
            let target = app.state.workspaces[0].tabs[target_tab].root_pane;
            seed_terminal_states(&mut app);
            let source_public = app.public_pane_id(0, source).unwrap();
            let target_public = app.public_pane_id(0, target).unwrap();
            let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

            let response = app.handle_pane_move(
                "req".into(),
                PaneMoveParams {
                    pane_id: source_public,
                    destination: PaneMoveDestination::Tab {
                        tab_id: target_tab_public,
                        target_pane_id: Some(target_public),
                        split: split.clone(),
                        ratio: None,
                    },
                    focus: true,
                },
            );

            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
            let pane_ids = app.state.workspaces[0].tabs[0].layout.pane_ids();
            assert_eq!(pane_ids.first() == Some(&source), moved_first, "{split:?}");
        }
    }

    #[test]
    fn api_pane_move_focus_preserves_shared_mode() {
        let mut app = app_with_linked_worktree();
        app.state.set_server_mode(Mode::Settings);
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        seed_terminal_states(&mut app);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.server_mode(), Mode::Settings);
        assert_eq!(app.state.copy_mode.expect("copy mode").pane_id, source);
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), source);
    }

    #[tokio::test]
    async fn key_release_follows_pane_moved_across_workspaces() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal_id = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80,
                24,
                0,
                b"\x1b[>15u",
                2,
            );
        app.terminal_runtimes.insert(source_terminal_id, runtime);
        app.state.workspaces.push(Workspace::test_new("other"));
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.set_server_mode(Mode::Terminal);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::empty(),
                ),
            )],
            false,
        );
        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id,
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(matches!(success.result, ResponseResult::PaneMove { .. }));
        app.route_client_events_from(
            42,
            vec![crate::raw_input::RawInputEvent::Key(
                crate::input::TerminalKey::new(
                    crossterm::event::KeyCode::Char('j'),
                    crossterm::event::KeyModifiers::empty(),
                )
                .with_kind(crossterm::event::KeyEventKind::Release),
            )],
            false,
        );

        assert_eq!(
            rx.try_recv().expect("forwarded press"),
            bytes::Bytes::from_static(b"\x1b[106;1:1u")
        );
        assert_eq!(
            rx.try_recv().expect("forwarded release after pane move"),
            bytes::Bytes::from_static(b"\x1b[106;1:3u")
        );
        assert!(app.input_leases.is_empty());
    }

    #[test]
    fn api_pane_move_to_existing_tab_across_workspace_reassigns_public_pane_id() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        app.state
            .terminals
            .get_mut(&source_terminal)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let previous_pane_id = app.public_pane_id(0, source).unwrap();
        let previous_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let target_pane_id = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: previous_pane_id.clone(),
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_id.clone(),
                    target_pane_id: Some(target_pane_id),
                    split: SplitDirection::Down,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.previous_pane_id, previous_pane_id);
        assert_eq!(move_result.previous_workspace_id, previous_workspace_id);
        assert_eq!(move_result.closed_workspace_id, Some(previous_workspace_id));
        assert_ne!(move_result.pane.pane_id, move_result.previous_pane_id);
        assert!(move_result
            .pane
            .pane_id
            .starts_with(&format!("{target_workspace_id}:p")));
        assert_eq!(move_result.pane.workspace_id, target_workspace_id);
        assert_eq!(move_result.pane.tab_id, target_tab_id);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(app.parse_pane_id(&previous_pane_id), Some((0, source)));
        assert!(matches!(
            app.resolve_agent_target(&previous_pane_id),
            Err(crate::app::terminal_targets::TerminalTargetError::NotFound { .. })
        ));
        assert!(app.resolve_agent_target(&move_result.pane.pane_id).is_ok());
    }

    #[test]
    fn api_pane_move_legacy_target_tab_id_survives_source_workspace_removal() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let target = app.state.workspaces[1].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_workspace_id = app.public_workspace_id(0);
        let target_workspace_id = app.public_workspace_id(1);
        let target_tab_id = app.public_tab_id(1, 0).unwrap();
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: "t_2_1".into(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(move_result.closed_workspace_id, Some(source_workspace_id));
        assert_eq!(move_result.pane.workspace_id, target_workspace_id);
        assert_eq!(move_result.pane.tab_id, target_tab_id);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
    }

    #[test]
    fn api_pane_move_to_new_tab_creates_tab_without_spawning_terminal() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("moved")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_eq!(move_result.closed_tab_id, None);
        assert_eq!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces[0].tabs.len(), 2);
        assert!(app.state.workspaces[0].tabs[0].terminal_id(right).is_some());
        assert_eq!(
            app.state.workspaces[0].tabs[1].terminal_id(source),
            Some(&source_terminal)
        );
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(
            events,
            vec![
                EventKind::TabCreated,
                EventKind::PaneMoved,
                EventKind::LayoutUpdated,
                EventKind::LayoutUpdated,
            ]
        );
        match &envelopes[0].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
        assert!(matches!(
            &envelopes[2].1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
        ));
        assert!(matches!(
            &envelopes[3].1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 1).unwrap()
        ));
    }

    #[test]
    fn api_pane_move_only_pane_to_new_tab_uses_app_render_handles() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::NewTab {
                    workspace_id: None,
                    label: Some("moved".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
    }

    #[test]
    fn api_pane_move_to_new_workspace_closes_empty_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_workspace = app.public_workspace_id(0);

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public.clone(),
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("promoted".into()),
                    tab_label: Some("main".into()),
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.closed_workspace_id, Some(source_workspace));
        assert_eq!(
            move_result
                .created_workspace
                .as_ref()
                .map(|ws| ws.label.as_str()),
            Some("promoted")
        );
        assert_eq!(
            move_result.created_workspace.as_ref().map(|ws| ws.focused),
            Some(true)
        );
        assert_eq!(
            move_result
                .created_tab
                .as_ref()
                .map(|tab| tab.label.as_str()),
            Some("main")
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(true)
        );
        assert_ne!(move_result.pane.pane_id, source_public);
        assert_eq!(move_result.pane.terminal_id, source_terminal.to_string());
        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_notify,
            &app.render_notify
        ));
        assert!(std::sync::Arc::ptr_eq(
            &app.state.workspaces[0].tabs[0].render_dirty,
            &app.render_dirty
        ));
        let envelopes = app.event_hub.events_after(0);
        let events: Vec<_> = envelopes
            .iter()
            .map(|(_, envelope)| envelope.event)
            .collect();
        assert_eq!(
            events,
            vec![
                EventKind::TabClosed,
                EventKind::WorkspaceClosed,
                EventKind::WorkspaceCreated,
                EventKind::TabCreated,
                EventKind::PaneMoved,
                EventKind::LayoutUpdated,
            ]
        );
        match &envelopes[2].1.data {
            EventData::WorkspaceCreated { workspace } => assert!(workspace.focused),
            other => panic!("expected workspace created event, got {other:?}"),
        }
        match &envelopes[3].1.data {
            EventData::TabCreated { tab } => assert!(tab.focused),
            other => panic!("expected tab created event, got {other:?}"),
        }
        match &envelopes[5].1.data {
            EventData::LayoutUpdated { layout } => assert_eq!(
                layout.tab_id,
                app.public_tab_id(0, 0)
                    .expect("created workspace should have a first tab")
            ),
            other => panic!("expected layout updated event, got {other:?}"),
        }
    }

    // An API-initiated move must never yank the human's cursor: with
    // `focus: false` the session focus stays in the source workspace even when
    // the moved pane is the pane the user was sitting in.
    #[test]
    fn api_pane_move_to_new_workspace_without_focus_keeps_session_focus_in_source() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let sibling = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        app.state.active = Some(0);
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_workspace = app.public_workspace_id(0);

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("agent".into()),
                    tab_label: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.closed_workspace_id, None);
        assert_eq!(app.public_workspace_id(0), source_workspace);
        assert_eq!(
            app.state.active,
            Some(0),
            "session focus must stay in the source workspace"
        );
        assert_eq!(app.state.workspaces[0].tabs[0].layout.focused(), sibling);
        assert_eq!(
            move_result.created_workspace.as_ref().map(|ws| ws.focused),
            Some(false)
        );
        assert_eq!(
            move_result.created_tab.as_ref().map(|tab| tab.focused),
            Some(false)
        );
        // `focused_pane_id` reports the destination layout, never session focus.
        assert_eq!(
            move_result.focused_pane_id,
            move_result.target_layout.focused_pane_id
        );
        assert_eq!(move_result.focused_pane_id, move_result.pane.pane_id);
    }

    // The explicit opt-in keeps working: `focus: true` still moves the session.
    #[test]
    fn api_pane_move_to_new_workspace_with_focus_moves_session_focus() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let _sibling = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(source);
        app.state.active = Some(0);
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::NewWorkspace {
                    label: Some("agent".into()),
                    tab_label: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert_eq!(app.state.active, Some(1));
        assert_eq!(
            move_result.created_workspace.as_ref().map(|ws| ws.focused),
            Some(true)
        );
    }

    #[test]
    fn api_pane_move_same_tab_returns_same_tab_noop() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let source_tab = app.public_tab_id(0, 0).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: source_tab,
                    target_pane_id: None,
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::SameTab));
        assert_eq!(app.state.workspaces[0].tabs.len(), 1);
    }

    #[test]
    fn api_pane_move_rejects_target_pane_outside_target_tab() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let other_tab = app.state.workspaces[0].test_add_tab(Some("other"));
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let wrong_target = app
            .public_pane_id(0, app.state.workspaces[0].tabs[other_tab].root_pane)
            .unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(wrong_target),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let error: crate::api::schema::ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "target_pane_not_found");
        assert_eq!(app.state.workspaces[0].tabs.len(), 3);
    }

    #[test]
    fn api_pane_move_existing_tab_no_focus_preserves_previous_target_focus() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let previously_focused = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].active_tab = target_tab;
        let explicit_target =
            app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[target_tab]
            .layout
            .focus_pane(previously_focused);
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let explicit_target_public = app.public_pane_id(0, explicit_target).unwrap();
        let previously_focused_public = app.public_pane_id(0, previously_focused).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public,
                    target_pane_id: Some(explicit_target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: false,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(move_result.changed);
        assert_eq!(move_result.focused_pane_id, previously_focused_public);
        assert_eq!(
            app.state.workspaces[0].tabs[0].layout.focused(),
            previously_focused
        );
    }

    #[test]
    fn api_pane_move_recovery_restores_removed_source_workspace() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let source_terminal = app.state.workspaces[0].tabs[0]
            .terminal_id(source)
            .unwrap()
            .clone();
        let previous_workspace_id = app.public_workspace_id(0);
        // The operator cleared this workspace's binding; recovery must not
        // hand it back in a state where adoption would rebind it.
        app.state.workspaces[0].repo_binding_cleared = true;
        let context = PaneMoveRecoveryContext {
            source_ws_idx: 0,
            previous_workspace_id: previous_workspace_id.clone(),
            previous_workspace_label: app.state.workspaces[0].custom_name.clone(),
            previous_tab_label: app.state.workspaces[0].tabs[0].custom_name.clone(),
            previous_worktree_space: app.state.workspaces[0].worktree_space.clone(),
            previous_repo_binding: app.state.workspaces[0].repo_binding.clone(),
            previous_repo_binding_cleared: app.state.workspaces[0].repo_binding_cleared,
            identity_cwd: app.state.workspaces[0].identity_cwd.clone(),
        };
        let taken = app.state.workspaces[0]
            .take_pane_for_move(source)
            .expect("source pane should be movable");
        app.state.workspaces.remove(0);
        app.state.active = None;
        app.state.selected = 0;

        app.recover_failed_pane_move(context, taken.moved);

        assert_eq!(app.state.workspaces.len(), 1);
        assert_eq!(app.state.workspaces[0].id, previous_workspace_id);
        assert_eq!(
            app.state.workspaces[0].tabs[0].terminal_id(source),
            Some(&source_terminal)
        );
        assert_eq!(
            app.parse_pane_id(&format!("{previous_workspace_id}:p1")),
            Some((0, source))
        );
        assert!(
            app.state.workspaces[0].repo_binding_cleared,
            "recovery must carry the cleared marker back"
        );
    }

    #[test]
    fn api_pane_move_to_zoomed_target_returns_target_layout() {
        let mut app = app_with_linked_worktree();
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target_tab = app.state.workspaces[0].test_add_tab(Some("target"));
        let target = app.state.workspaces[0].tabs[target_tab].root_pane;
        app.state.workspaces[0].tabs[target_tab].zoomed = true;
        seed_terminal_states(&mut app);
        let source_public = app.public_pane_id(0, source).unwrap();
        let target_tab_public = app.public_tab_id(0, target_tab).unwrap();
        let target_public = app.public_pane_id(0, target).unwrap();

        let response = app.handle_pane_move(
            "req".into(),
            PaneMoveParams {
                pane_id: source_public,
                destination: PaneMoveDestination::Tab {
                    tab_id: target_tab_public.clone(),
                    target_pane_id: Some(target_public),
                    split: SplitDirection::Right,
                    ratio: None,
                },
                focus: true,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneMove { move_result } = success.result else {
            panic!("expected pane move response");
        };
        assert!(!move_result.changed);
        assert_eq!(move_result.reason, Some(PaneMoveReason::ZoomedTab));
        assert_eq!(move_result.target_layout.tab_id, target_tab_public);
        assert_eq!(
            move_result
                .source_layout
                .as_ref()
                .map(|layout| layout.tab_id.as_str()),
            app.public_tab_id(0, 0).as_deref()
        );
    }

    #[test]
    fn api_pane_zoom_current_toggles_zoom() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, None);
        assert_eq!(zoom.pane_id, root_public);
        assert_eq!(zoom.focused_pane_id, zoom.pane_id);
        assert!(zoom.zoomed);
        assert!(zoom.layout.zoomed);
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap() && layout.zoomed
        ));

        let response = app.handle_pane_zoom("req".into(), PaneZoomParams::default());
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);
        assert!(!zoom.layout.zoomed);
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap() && !layout.zoomed
        ));
    }

    #[test]
    fn api_pane_zoom_explicit_background_pane_updates_focus_history() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let first = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.workspaces[0].tabs[0].layout.focus_pane(first);
        let target_public = app.public_pane_id(1, target).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(target_public.clone()),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
        assert!(app.state.workspaces[1].tabs[0].zoomed);

        app.state.last_pane();

        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(first));
    }

    #[test]
    fn api_pane_zoom_focuses_copy_mode_pane_back_into_copy_mode() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let source = app.state.workspaces[0].tabs[0].root_pane;
        let target = app.state.workspaces[1].tabs[0].root_pane;
        let _other = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let _target_other =
            app.state.workspaces[1].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[1].tabs[0].layout.focus_pane(target);
        app.state.active = Some(1);
        app.state.selected = 1;
        app.state.set_server_mode(Mode::Terminal);
        app.state.copy_mode = Some(crate::app::state::CopyModeState {
            pane_id: source,
            cursor_row: 0,
            cursor_col: 0,
            entry_offset_from_bottom: 0,
            selection: None,
            search: Default::default(),
        });
        let source_public = app.public_pane_id(0, source).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(source_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.focus_changed);
        assert_eq!(app.state.active, Some(0));
        assert_eq!(app.state.server_mode(), Mode::Copy);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(source));
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target));
    }

    #[test]
    fn api_pane_zoom_single_pane_returns_noop() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::Toggle,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::SinglePane));
        assert_eq!(zoom.pane_id, root_public);
        assert!(!zoom.zoomed);
        assert!(!app.state.workspaces[0].tabs[0].zoomed);
    }

    #[test]
    fn api_pane_zoom_on_and_off_are_idempotent() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let _right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public.clone()),
                mode: PaneZoomMode::On,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(root_public),
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert!(!zoom.zoomed);

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: None,
                mode: PaneZoomMode::Off,
            },
        );
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(!zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(!zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyUnzoomed));
        assert!(!zoom.zoomed);
    }

    #[test]
    fn api_pane_zoom_idempotent_mode_reports_focus_change() {
        let mut app = app_with_linked_worktree();
        app.state.active = Some(0);
        app.state.selected = 0;
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        app.state.workspaces[0].tabs[0].zoomed = true;
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_zoom(
            "req".into(),
            PaneZoomParams {
                pane_id: Some(right_public),
                mode: PaneZoomMode::On,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneZoom { zoom } = success.result else {
            panic!("expected pane zoom response");
        };
        assert!(zoom.changed);
        assert!(!zoom.zoom_changed);
        assert!(zoom.focus_changed);
        assert_eq!(zoom.reason, Some(PaneZoomReason::AlreadyZoomed));
        assert!(zoom.zoomed);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.focused_pane_id == app.public_pane_id(0, right).unwrap()
        ));
    }

    #[test]
    fn api_pane_zoom_params_serialize_modes() {
        let request = crate::api::schema::Request {
            id: "req".into(),
            method: crate::api::schema::Method::PaneZoom(PaneZoomParams {
                pane_id: Some("issue-1".into()),
                mode: PaneZoomMode::On,
            }),
        };

        let encoded = serde_json::to_string(&request).unwrap();
        assert!(encoded.contains("\"method\":\"pane.zoom\""));
        assert!(encoded.contains("\"mode\":\"on\""));

        let decoded: crate::api::schema::Request = serde_json::from_str(&encoded).unwrap();
        let crate::api::schema::Method::PaneZoom(params) = decoded.method else {
            panic!("expected pane zoom request");
        };
        assert_eq!(params.pane_id, Some("issue-1".into()));
        assert_eq!(params.mode, PaneZoomMode::On);
    }

    #[test]
    fn api_pane_layout_returns_public_ids_rects_and_splits() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_layout(
            "req".into(),
            crate::api::schema::PaneLayoutParams {
                pane_id: Some(root_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneLayout { layout } = success.result else {
            panic!("expected pane layout response");
        };
        assert_eq!(layout.focused_pane_id, root_public);
        assert!(layout.panes.iter().any(|pane| pane.pane_id == root_public));
        assert!(layout.panes.iter().any(|pane| pane.pane_id == right_public));
        assert_eq!(layout.splits.len(), 1);
        assert_eq!(
            layout.splits[0].direction,
            crate::api::schema::SplitDirection::Right
        );
    }

    #[test]
    fn api_pane_neighbor_returns_directional_neighbor_public_id() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_neighbor(
            "req".into(),
            crate::api::schema::PaneNeighborParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneNeighbor { neighbor } = success.result else {
            panic!("expected pane neighbor response");
        };
        assert_eq!(neighbor.pane_id, root_public);
        assert_eq!(neighbor.direction, PaneDirection::Right);
        assert_eq!(neighbor.neighbor_pane_id, Some(right_public));
    }

    #[test]
    fn api_pane_edges_reports_physical_layout_edges() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_edges(
            "req".into(),
            crate::api::schema::PaneEdgesParams {
                pane_id: Some(right_public.clone()),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneEdges { edges } = success.result else {
            panic!("expected pane edges response");
        };
        assert_eq!(edges.pane_id, right_public);
        assert!(!edges.left);
        assert!(edges.right);
        assert!(edges.up);
        assert!(edges.down);
    }

    #[test]
    fn api_pane_resize_changes_target_ratio_without_changing_focus() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(right);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_resize(
            "req".into(),
            crate::api::schema::PaneResizeParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
                amount: Some(0.1),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneResize { resize } = success.result else {
            panic!("expected pane resize response");
        };
        assert!(resize.changed);
        assert_eq!(resize.reason, None);
        assert_eq!(resize.pane_id, root_public);
        assert_eq!(resize.focused_pane_id, right_public);
        assert_eq!(resize.layout.focused_pane_id, right_public);
        assert!((resize.layout.splits[0].ratio - 0.6).abs() < f32::EPSILON);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
        assert!(matches!(
            &app.event_hub.events_after(0).last().expect("layout event").1.data,
            EventData::LayoutUpdated { layout }
                if layout.tab_id == app.public_tab_id(0, 0).unwrap()
                    && (layout.splits[0].ratio - 0.6).abs() < f32::EPSILON
        ));
    }

    #[test]
    fn api_pane_focus_direction_focuses_neighbor() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        let right = app.state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();
        let right_public = app.public_pane_id(0, right).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Right,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(focus.changed);
        assert_eq!(focus.reason, None);
        assert_eq!(focus.source_pane_id, root_public);
        assert_eq!(focus.focused_pane_id, Some(right_public.clone()));
        assert_eq!(focus.layout.focused_pane_id, right_public);
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(right));
    }

    #[test]
    fn api_pane_focus_focuses_direct_target_across_tabs_and_workspaces() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces.push(Workspace::test_new("other"));
        let target_tab_idx = app.state.workspaces[1].test_add_tab(Some("target"));
        app.state.workspaces[1].switch_tab(target_tab_idx);
        let target_pane = app.state.workspaces[1].tabs[target_tab_idx].root_pane;
        app.state.ensure_test_terminals();
        let target_public = app.public_pane_id(1, target_pane).unwrap();
        app.state.switch_workspace(0);
        app.state.set_server_mode(Mode::Settings);
        assert_eq!(app.state.active, Some(0));

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: target_public.clone(),
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        assert_eq!(pane.pane_id, target_public);
        assert_eq!(app.state.active, Some(1));
        assert_eq!(app.state.workspaces[1].active_tab, target_tab_idx);
        assert_eq!(app.state.workspaces[1].focused_pane_id(), Some(target_pane));
        assert_eq!(app.state.server_mode(), Mode::Settings);
    }

    #[test]
    fn api_pane_focus_marks_already_focused_done_pane_seen() {
        let mut app = app_with_linked_worktree();
        app.state.workspaces[0].test_add_tab(Some("later"));
        let tab_order = app.state.workspaces[0]
            .tabs
            .iter()
            .map(|tab| tab.number)
            .collect::<Vec<_>>();
        app.state.active = Some(0);
        app.state.selected = 0;
        app.state.outer_terminal_focus = Some(false);

        let pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = app.state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_raw_agent_state_for_test(crate::detect::AgentState::Idle);
        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .unwrap()
            .seen = false;
        app.state.workspaces[0].tabs[0].layout.focus_pane(pane_id);

        let public_pane_id = app.public_pane_id(0, pane_id).unwrap();
        let response = app.handle_pane_focus(
            "req".into(),
            PaneTarget {
                pane_id: public_pane_id,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info response");
        };
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Idle);
        assert!(app.state.workspaces[0].tabs[0].panes[&pane_id].seen);
        assert_eq!(
            app.state.workspaces[0]
                .tabs
                .iter()
                .map(|tab| tab.number)
                .collect::<Vec<_>>(),
            tab_order,
            "focusing completed work must not reorder tabs"
        );
    }

    #[test]
    fn api_pane_focus_rejects_invalid_pane_id() {
        let mut app = app_with_linked_worktree();

        let response = app.handle_pane_focus(
            "req".into(),
            crate::api::schema::PaneTarget {
                pane_id: "pane_missing".into(),
            },
        );

        let error: ErrorResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(error.error.code, "pane_not_found");
    }

    #[test]
    fn api_pane_focus_direction_no_neighbor_is_noop() {
        let mut app = app_with_linked_worktree();
        let root = app.state.workspaces[0].tabs[0].root_pane;
        app.state.workspaces[0].tabs[0].layout.focus_pane(root);
        crate::ui::compute_view(&mut app.state, ratatui::layout::Rect::new(0, 0, 100, 20));
        let root_public = app.public_pane_id(0, root).unwrap();

        let response = app.handle_pane_focus_direction(
            "req".into(),
            crate::api::schema::PaneFocusDirectionParams {
                pane_id: Some(root_public.clone()),
                direction: PaneDirection::Left,
            },
        );

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneFocusDirection { focus } = success.result else {
            panic!("expected pane focus direction response");
        };
        assert!(!focus.changed);
        assert_eq!(focus.reason, Some(PaneFocusDirectionReason::NoNeighbor));
        assert_eq!(focus.source_pane_id, root_public.clone());
        assert_eq!(focus.focused_pane_id, Some(root_public));
        assert_eq!(app.state.workspaces[0].focused_pane_id(), Some(root));
    }

    #[test]
    fn pane_metadata_tokens_patch_and_clear_through_dispatcher() {
        let (mut app, pane_id) = app_with_test_workspace();
        for (tokens, expected) in [
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("reviewing auth".into())),
                    ("model".into(), Some("opus".into())),
                ]),
                std::collections::HashMap::from([
                    ("summary".into(), "reviewing auth".into()),
                    ("model".into(), "opus".into()),
                ]),
            ),
            (
                std::collections::HashMap::from([
                    ("summary".into(), Some("done".into())),
                    ("model".into(), None),
                ]),
                std::collections::HashMap::from([("summary".into(), "done".into())]),
            ),
        ] {
            let mut params = metadata_params(pane_id.clone());
            params.title = None;
            params.tokens = tokens;
            let response = app.handle_api_request(crate::api::schema::Request {
                id: "set".into(),
                method: crate::api::schema::Method::PaneReportMetadata(params),
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            assert_eq!(success.result, ResponseResult::Ok {});

            let response = app.handle_api_request(crate::api::schema::Request {
                id: "get".into(),
                method: crate::api::schema::Method::PaneGet(PaneTarget {
                    pane_id: pane_id.clone(),
                }),
            });
            let success: SuccessResponse = serde_json::from_str(&response).unwrap();
            let ResponseResult::PaneInfo { pane } = success.result else {
                panic!("expected pane info");
            };
            assert_eq!(pane.tokens, expected);
        }
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert!(app.state.terminals[&terminal_id].agent_metadata.is_empty());
    }

    #[test]
    fn closing_block_metadata_replaces_contract_state_and_clears_absent_tokens() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let source = "herdr:claude-closing-block";
        let mut report = metadata_params(pane_id.clone());
        report.title = None;
        report.source = source.into();
        report.applies_to_source = Some(source.into());
        report.seq = Some(1);
        report.tokens = std::collections::HashMap::from([
            ("closing_idle".into(), Some("1".into())),
            ("closing_contract".into(), Some("x".repeat(200))),
            ("closing_contract_met".into(), Some("1".into())),
        ]);
        assert!(app
            .handle_pane_report_metadata("contract".into(), report)
            .contains("\"result\""));

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.closing_contract.as_deref(),
            Some("x".repeat(200).as_str())
        );
        assert_eq!(terminal.closing_contract_met, Some(true));
        assert!(terminal.closing_contract_met_at.is_some());

        let mut clear = metadata_params(pane_id.clone());
        clear.title = None;
        clear.source = source.into();
        clear.applies_to_source = Some(source.into());
        clear.seq = Some(2);
        clear.tokens = std::collections::HashMap::from([("closing_idle".into(), Some("1".into()))]);
        app.handle_pane_report_metadata("clear".into(), clear);
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.closing_idle, Some(true));
        assert!(terminal.closing_contract.is_none());
        assert!(terminal.closing_contract_met.is_none());
        assert!(terminal.closing_contract_met_at.is_none());

        let mut untrusted = metadata_params(pane_id);
        untrusted.title = None;
        untrusted.source = "user:metadata.test-1".into();
        untrusted.seq = Some(1);
        untrusted.tokens = std::collections::HashMap::from([
            ("closing_idle".into(), Some("1".into())),
            ("closing_contract".into(), Some("spoofed".into())),
            ("closing_contract_met".into(), Some("1".into())),
        ]);
        app.handle_pane_report_metadata("untrusted".into(), untrusted);
        assert!(app.state.terminals[&terminal_id].closing_contract.is_none());
    }

    #[test]
    fn pane_report_agent_working_transition_stamps_agent_activity_once() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);
        let report = |seq| PaneReportAgentParams {
            pane_id: pane_id.clone(),
            source: "custom:hook-test".into(),
            agent: "pi".into(),
            state: crate::api::schema::PaneAgentState::Working,
            v: None,
            message: None,
            seq: Some(seq),
            wait: None,
            eta_s: None,
            reported_at: None,
            agent_session_id: None,
            agent_session_path: None,
            gates: None,
            items: None,
            decisions: None,
            completion: None,
            external_wait: None,
            parse_status: None,
            workers_unknown: None,
            agents: None,
            last_turn_at: None,
            settle_ready: None,
        };

        let response = app.handle_pane_report_agent("working-1".into(), report(1));
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let first = app
            .state
            .agent_states
            .snapshot(internal_pane_id, crate::api::schema::AgentStatus::Working)
            .last_acted_at
            .expect("accepted transition into working must stamp receipt time");

        std::thread::sleep(std::time::Duration::from_millis(2));
        let response = app.handle_pane_report_agent("working-2".into(), report(2));
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let repeated = app
            .state
            .agent_states
            .snapshot(internal_pane_id, crate::api::schema::AgentStatus::Working)
            .last_acted_at
            .expect("working activity timestamp");
        assert_eq!(repeated, first, "repeated working is not a transition");
    }

    #[test]
    fn pane_report_agent_v2_closing_block_arrays_are_exposed_without_pty() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let gates = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 1,
            label: "Gate".into(),
            text: "Approve the open Herdr PR".into(),
            pr: Some(2606),
            ticket: Some("MAT-125".into()),
            url: Some("https://mat125-gates-v2.vercel.app".into()),
            default: None,
            default_at: None,
        }];
        let items = vec![crate::api::schema::ClosingBlockItem {
            blocking: true,
            n: 2,
            label: "Answer".into(),
            text: "Use the fork PR".into(),
            pr: None,
            ticket: None,
            url: None,
            default: None,
            default_at: None,
        }];
        let decisions = vec![crate::api::schema::ClosingBlockDecision {
            n: 1,
            text: "Proceed on the lane".into(),
            recommendation: "proceed".into(),
            reversible: true,
            decided_at: "2026-08-09T12:00:00Z".into(),
        }];
        let response = app.handle_pane_report_agent(
            "closing-block-v2".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: Some(2),
                message: Some("Approve the open Herdr PR".into()),
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(gates),
                items: Some(items),
                decisions: Some(decisions),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "get".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget {
                pane_id: pane_id.clone(),
            }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert_eq!(pane.gates.len(), 1);
        assert_eq!(pane.gates[0].text, "Approve the open Herdr PR");
        assert_eq!(pane.gates[0].pr, Some(2606));
        assert_eq!(pane.items[0].label, "Answer");
        assert_eq!(pane.decisions[0].recommendation, "proceed");
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Blocked);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn blocked_closing_gate_with_external_wait_is_blocked_in_every_agent_read() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        app.terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b""),
        );

        let response = app.handle_pane_report_agent(
            "blocked-with-wait".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: Some(crate::api::schema::panes::CLOSING_BLOCK_VERSION),
                message: Some("Approve the release after CI".into()),
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(vec![test_gate()]),
                items: Some(Vec::new()),
                decisions: Some(Vec::new()),
                completion: Some(crate::api::schema::ClosingCompletion::Incomplete),
                external_wait: Some("CI run 4123".into()),
                parse_status: Some(crate::api::schema::ClosingParseStatus::Ok),
                workers_unknown: Some(false),
                agents: Some(1),
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let explain: serde_json::Value = serde_json::from_str(&app.handle_agent_explain(
            "explain".into(),
            crate::api::schema::AgentTarget {
                target: pane_id.clone(),
            },
        ))
        .unwrap();
        assert_eq!(explain["result"]["explain"]["screen_state"], "idle");
        assert_eq!(explain["result"]["explain"]["state"], "blocked");
        assert_eq!(explain["result"]["explain"]["effective_state"], "blocked");

        let get: SuccessResponse = serde_json::from_str(&app.handle_agent_get(
            "get".into(),
            crate::api::schema::AgentTarget {
                target: pane_id.clone(),
            },
        ))
        .unwrap();
        let ResponseResult::AgentInfo { agent: get_agent } = get.result else {
            panic!("expected agent info");
        };
        assert_eq!(
            get_agent.agent_status,
            crate::api::schema::AgentStatus::Blocked
        );

        let list: SuccessResponse =
            serde_json::from_str(&app.handle_agent_list("list".into())).unwrap();
        let ResponseResult::AgentList { agents } = list.result else {
            panic!("expected agent list");
        };
        assert_eq!(
            agents[0].agent_status,
            crate::api::schema::AgentStatus::Blocked
        );

        app.state.fleet_snapshot = crate::fleet::Snapshot {
            polled: true,
            hosts: vec![crate::fleet::HostSnapshot {
                name: "remote".into(),
                target: "remote".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                entries: vec![crate::fleet::FleetRow::test_agent_info_row(
                    "remote", get_agent,
                )],
            }],
            ..crate::fleet::Snapshot::default()
        };
        let fleet: serde_json::Value =
            serde_json::from_str(&app.handle_fleet_list("fleet".into())).unwrap();
        assert_eq!(
            fleet["result"]["snapshot"]["hosts"][0]["agents"][0]["state"],
            "blocked"
        );
    }

    #[test]
    fn approve_gate_and_decide_item_count_as_closing_human_input() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let mut report = closing_block_report(&pane_id, 1, vec![test_attention_item("Approve")]);
        report.state = crate::api::schema::PaneAgentState::Blocked;
        report.items = Some(vec![test_attention_item("Decide")]);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("approve-decide".into(), report))
                .unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.has_pending_human_input());
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_blocking")
                .map(String::as_str),
            Some("2")
        );
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
    }

    #[test]
    fn nonblocking_closing_report_stays_blocked_until_pane_input() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), crate::detect::AgentState::Idle);

        let mut blocked = closing_block_report(&pane_id, 1, vec![test_gate()]);
        blocked.state = crate::api::schema::PaneAgentState::Blocked;
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("blocked".into(), blocked)).unwrap();
        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent(
            "working-report".into(),
            closing_block_report(&pane_id, 2, Vec::new()),
        ))
        .unwrap();

        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
        assert_eq!(
            app.state.terminals[&terminal_id].closing_gates(),
            &[test_gate()]
        );

        app.retire_blocked_hook_authority_for_pane(internal_pane_id, std::time::Instant::now());
        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent(
            "after-input".into(),
            closing_block_report(&pane_id, 3, Vec::new()),
        ))
        .unwrap();

        assert_ne!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
    }

    #[test]
    fn missing_short_reply_preserves_the_previous_human_blocker() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let mut blocked = closing_block_report(&pane_id, 1, vec![test_gate()]);
        blocked.state = crate::api::schema::PaneAgentState::Blocked;
        blocked.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        blocked.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("blocked".into(), blocked)).unwrap();
        let mut blocked_metadata = metadata_params(pane_id.clone());
        blocked_metadata.title = None;
        blocked_metadata.source = "herdr:codex-closing-block".into();
        blocked_metadata.applies_to_source = Some("herdr:codex-closing-block".into());
        blocked_metadata.seq = Some(1);
        blocked_metadata.tokens = std::collections::HashMap::from([
            ("closing_blocking".into(), Some("1".into())),
            (
                "closing_gates".into(),
                Some("Choose the release path".into()),
            ),
            ("closing_completion".into(), Some("incomplete".into())),
            ("closing_parse".into(), Some("ok".into())),
        ]);
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("blocked-metadata".into(), blocked_metadata),
        )
        .unwrap();

        let mut progressing = closing_block_report(&pane_id, 2, Vec::new());
        progressing.state = crate::api::schema::PaneAgentState::Unknown;
        progressing.completion = Some(crate::api::schema::ClosingCompletion::Missing);
        progressing.parse_status = Some(crate::api::schema::ClosingParseStatus::Missing);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("progressing".into(), progressing))
                .unwrap();
        let mut missing_metadata = metadata_params(pane_id.clone());
        missing_metadata.title = None;
        missing_metadata.source = "herdr:codex-closing-block".into();
        missing_metadata.applies_to_source = Some("herdr:codex-closing-block".into());
        missing_metadata.seq = Some(2);
        missing_metadata.tokens = std::collections::HashMap::from([
            ("closing_blocking".into(), Some("0".into())),
            ("closing_gates".into(), None),
            ("closing_completion".into(), Some("missing".into())),
            ("closing_parse".into(), Some("missing".into())),
        ]);
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("missing-metadata".into(), missing_metadata),
        )
        .unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.closing_gates, vec![test_gate()]);
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_blocking")
                .map(String::as_str),
            Some("1")
        );
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_gates")
                .map(String::as_str),
            Some("Choose the release path")
        );
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked
        );
    }

    #[test]
    fn malformed_report_preserves_authoritative_dependencies() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let gate = test_gate();
        let mut answer = gate.clone();
        answer.label = "Answer".into();
        answer.text = "What next for placement accuracy?".into();
        answer.blocking = false;
        let decision = crate::api::schema::ClosingBlockDecision {
            n: 1,
            text: "Keep the current lane".into(),
            recommendation: "keep".into(),
            reversible: true,
            decided_at: "2026-09-17T12:00:00Z".into(),
        };
        let mut valid = closing_block_report(&pane_id, 1, vec![gate.clone()]);
        valid.items = Some(vec![answer.clone()]);
        valid.decisions = Some(vec![decision.clone()]);
        valid.agents = Some(2);
        valid.state = crate::api::schema::PaneAgentState::Working;
        valid.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        valid.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        valid.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("valid".into(), valid)).unwrap();

        let mut newly_recognized_answer = answer.clone();
        newly_recognized_answer.n = 2;
        newly_recognized_answer.text = "Choose the placement lane".into();
        let mut malformed = closing_block_report(&pane_id, 2, Vec::new());
        malformed.gates = None;
        malformed.items = Some(vec![newly_recognized_answer.clone()]);
        malformed.decisions = None;
        malformed.agents = Some(0);
        malformed.state = crate::api::schema::PaneAgentState::Unknown;
        malformed.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        malformed.parse_status = Some(crate::api::schema::ClosingParseStatus::Malformed);
        malformed.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("malformed".into(), malformed))
                .unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.closing_gates, vec![gate]);
        assert_eq!(
            terminal.closing_items,
            vec![answer, newly_recognized_answer]
        );
        assert_eq!(terminal.closing_decisions, vec![decision]);
        assert_eq!(terminal.effective_active_subagents(), Some(2));
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Blocked,
            "a malformed report cannot answer the live gate"
        );
    }

    #[test]
    fn api_republishes_blocking_from_the_effective_item_label() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let params: PaneReportAgentParams = serde_json::from_value(serde_json::json!({
            "pane_id": pane_id,
            "source": "herdr:codex-closing-block",
            "agent": "codex",
            "state": "idle",
            "v": 2,
            "seq": 1,
            "gates": [],
            "items": [{
                "n": 1,
                "label": "Answer",
                "text": "Choose the release path",
                "blocking": false
            }],
            "decisions": [],
            "completion": "incomplete",
            "parse_status": "ok",
            "workers_unknown": false,
            "agents": 0
        }))
        .expect("adapter payload should deserialize");

        let response = app.handle_pane_report_agent("contradictory-item".into(), params);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let pane = app.pane_info(0, internal_pane_id).expect("pane info");

        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Blocked);
        assert!(pane.items[0].blocking);
    }

    #[test]
    fn registered_external_wait_projects_as_in_progress_without_an_eta() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let mut waiting = closing_block_report(&pane_id, 1, Vec::new());
        waiting.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        waiting.external_wait = Some("CI run 4123".into());
        waiting.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        waiting.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("external-wait".into(), waiting))
                .unwrap();

        let pane = app.pane_info(0, internal_pane_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(pane.wait.as_deref(), Some("CI run 4123"));
        assert_eq!(
            pane.tokens.get("closing_wait").map(String::as_str),
            Some("CI run 4123")
        );

        let stale_after = std::time::Duration::from_secs(20 * 60);
        let elapsed = std::time::Instant::now() + std::time::Duration::from_secs(24 * 60 * 60);
        assert!(app
            .state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .mark_agent_status_stale_at(elapsed, stale_after)
            .is_none());
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Working,
            "an elapsed registered wait remains in progress until its wake report"
        );

        let mut missing = closing_block_report(&pane_id, 2, Vec::new());
        missing.state = crate::api::schema::PaneAgentState::Unknown;
        missing.completion = Some(crate::api::schema::ClosingCompletion::Missing);
        missing.parse_status = Some(crate::api::schema::ClosingParseStatus::Missing);
        missing.workers_unknown = Some(false);
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("missing-after-wait".into(), missing),
        )
        .unwrap();

        let mut missing_metadata = metadata_params(pane_id.clone());
        missing_metadata.title = None;
        missing_metadata.source = "herdr:codex-closing-block".into();
        missing_metadata.applies_to_source = Some("herdr:codex-closing-block".into());
        missing_metadata.seq = Some(1);
        missing_metadata.tokens = std::collections::HashMap::from([
            ("closing_completion".into(), Some("missing".into())),
            ("closing_parse".into(), Some("missing".into())),
            ("closing_wait".into(), None),
            ("closing_workers_unknown".into(), Some("0".into())),
        ]);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata(
                "missing-metadata-after-wait".into(),
                missing_metadata,
            ))
            .unwrap();
        let pane = app.pane_info(0, internal_pane_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(pane.wait.as_deref(), Some("CI run 4123"));
        assert_eq!(
            pane.tokens.get("closing_wait").map(String::as_str),
            Some("CI run 4123")
        );

        let mut completed = closing_block_report(&pane_id, 3, Vec::new());
        completed.completion = Some(crate::api::schema::ClosingCompletion::Complete);
        completed.external_wait = Some(" \n\t ".into());
        completed.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        completed.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("completed".into(), completed))
                .unwrap();

        let pane = app.pane_info(0, internal_pane_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Idle);
        assert_eq!(pane.wait, None);
        assert!(!pane.tokens.contains_key("closing_wait"));
    }

    #[test]
    fn incomplete_idle_report_emits_only_the_final_unknown_status() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        let mut report = closing_block_report(&pane_id, 1, Vec::new());
        report.state = crate::api::schema::PaneAgentState::Idle;
        report.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        report.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        report.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("incomplete-idle".into(), report))
                .unwrap();

        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Unknown
        );
        let statuses = app
            .event_hub
            .events_after(0)
            .iter()
            .filter_map(|(_, event)| match &event.data {
                crate::api::schema::EventData::PaneAgentStatusChanged { agent_status, .. } => {
                    Some(*agent_status)
                }
                _ => None,
            })
            .collect::<Vec<_>>();
        assert_eq!(statuses, vec![crate::api::schema::AgentStatus::Unknown]);
    }

    #[test]
    fn installed_v2_idle_payload_keeps_the_legacy_done_projection() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(Some(1));

        // Captured from the installed v2 adapter. Only request-routing fields
        // are added; lifecycle fields are intentionally absent.
        let legacy_payload = serde_json::json!({
            "pane_id": pane_id,
            "source": "herdr:claude-closing-block",
            "agent": "claude",
            "agent_names": [],
            "agents": 0,
            "blocking": 0,
            "decisions": [],
            "gates": [],
            "items": [],
            "reported_at": "2026-09-17T13:10:04.914583Z",
            "seq": 1789650604914579484_u64,
            "state": "idle",
            "v": 2
        });
        let report: PaneReportAgentParams =
            serde_json::from_value(legacy_payload).expect("installed v2 payload parses");
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("legacy-idle".into(), report))
                .unwrap();

        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Working
        );
        assert_eq!(
            app.state.terminals[&terminal_id].verified_active_subagents(),
            Some(1)
        );
    }

    #[test]
    fn missing_report_preserves_an_unknown_worker_claim_through_metadata() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let mut unknown_worker = closing_block_report(&pane_id, 1, Vec::new());
        unknown_worker.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        unknown_worker.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        unknown_worker.workers_unknown = Some(true);
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("unknown-worker".into(), unknown_worker),
        )
        .unwrap();

        let mut missing = closing_block_report(&pane_id, 2, Vec::new());
        missing.state = crate::api::schema::PaneAgentState::Unknown;
        missing.completion = Some(crate::api::schema::ClosingCompletion::Missing);
        missing.parse_status = Some(crate::api::schema::ClosingParseStatus::Missing);
        missing.workers_unknown = Some(false);
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("missing-after-worker".into(), missing),
        )
        .unwrap();

        let mut missing_metadata = metadata_params(pane_id.clone());
        missing_metadata.title = None;
        missing_metadata.source = "herdr:codex-closing-block".into();
        missing_metadata.applies_to_source = Some("herdr:codex-closing-block".into());
        missing_metadata.seq = Some(1);
        missing_metadata.tokens = std::collections::HashMap::from([
            ("closing_completion".into(), Some("missing".into())),
            ("closing_parse".into(), Some("missing".into())),
            ("closing_workers_unknown".into(), Some("0".into())),
        ]);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata(
                "missing-metadata-after-worker".into(),
                missing_metadata,
            ))
            .unwrap();

        let pane = app.pane_info(0, internal_pane_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Unknown);
        assert_eq!(
            pane.tokens
                .get("closing_workers_unknown")
                .map(String::as_str),
            Some("1")
        );
    }

    #[cfg(unix)]
    #[test]
    fn unknown_adapter_workers_cannot_mask_a_live_native_subagent() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:claude",
            "claude",
            "session-current",
        );
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Idle);
        terminal.set_active_subagents(Some(1));

        let requests = closing_block_text_adapter_requests(
            &pane_id,
            "session-current",
            "**Critical action points (0 blocking)**\n\n\
             1 agent running: reviewer — checking the patch.\n",
        );
        let adapter_params = requests
            .iter()
            .find(|request| request["method"] == "pane.report_agent")
            .expect("adapter agent report")["params"]
            .clone();
        assert!(
            adapter_params.get("agents").is_none(),
            "unknown workers must not be serialized as a verified zero"
        );
        apply_closing_block_adapter_requests(&mut app, "unknown-workers", requests);

        let mut unverified_zero = adapter_params;
        unverified_zero["agents"] = serde_json::json!(0);
        unverified_zero["seq"] = serde_json::json!(unverified_zero["seq"]
            .as_u64()
            .expect("adapter sequence")
            .saturating_add(1));
        let unverified_zero: PaneReportAgentParams =
            serde_json::from_value(unverified_zero).expect("unverified count report parses");
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("unverified-zero".into(), unverified_zero),
        )
        .unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        let pane = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .expect("reported pane");
        assert_eq!(
            pane.agent_projection(terminal).state,
            AgentState::Working,
            "an unverified adapter count must not override the native live count"
        );
        assert_eq!(terminal.verified_active_subagents(), Some(1));
        assert!(!crate::app::pane_lifecycle::pane_is_done(pane, terminal));
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn adapter_answer_payload_needs_attention_and_clears_next_turn() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:claude",
            "claude",
            "session-current",
        );

        let requests = closing_block_adapter_requests(
            &pane_id,
            "session-current",
            serde_json::json!([{
                "n": 1,
                "label": "Answer",
                "text": "Choose the release lane"
            }]),
        );
        apply_closing_block_adapter_requests(&mut app, "answer-action-point", requests);

        app.state.workspaces[0].tabs[0]
            .panes
            .get_mut(&internal_pane_id)
            .expect("reported pane")
            .seen = false;

        let pane = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .expect("reported pane");
        let terminal = &app.state.terminals[&terminal_id];
        let projection = pane.agent_projection(terminal);
        assert_eq!(projection.state, AgentState::Blocked);
        assert_eq!(terminal.closing_items()[0].text, "Choose the release lane");
        let pane_info = app
            .pane_info(0, internal_pane_id)
            .expect("reported pane info");
        assert_eq!(pane_info.items[0].text, "Choose the release lane");
        assert!(projection.counts_as_blocked());
        assert_eq!(
            projection.attention_tier,
            crate::terminal::state::AttentionTier::Blocked
        );
        assert!(!crate::app::pane_lifecycle::pane_is_done(pane, terminal));

        app.state.auto_settle_inactive = true;
        app.state.settle_after = std::time::Duration::ZERO;
        let now = std::time::Instant::now();
        assert_eq!(app.state.refresh_settled_panes_at(None, now, 1), 0);
        assert!(!app.state.pane_is_settled(0, internal_pane_id));

        app.state.auto_nudge_stalled_agents = true;
        app.state.nudge_after = std::time::Duration::ZERO;
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .supervisor_stale = true;
        let (runtime, mut rx) =
            crate::terminal::TerminalRuntime::test_with_channel_and_scrollback_bytes(
                80, 24, 1024, b"", 8,
            );
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);
        assert!(!app.tick_auto_nudges(now));
        assert!(
            rx.try_recv().is_err(),
            "blocked pane received an auto-nudge"
        );

        app.retire_blocked_hook_authority_for_pane(internal_pane_id, std::time::Instant::now());
        let requests =
            closing_block_adapter_requests(&pane_id, "session-current", serde_json::json!([]));
        apply_closing_block_adapter_requests(&mut app, "clear-action-point", requests);
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.raw_agent_state(), AgentState::Idle);
        assert!(terminal.closing_gates().is_empty());
        assert!(terminal.closing_items().is_empty());
    }

    #[cfg(unix)]
    #[test]
    fn adapter_informational_item_stays_visible_without_blocking_done() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);
        bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:claude",
            "claude",
            "session-current",
        );
        app.state.active = None;
        app.state.workspaces[0]
            .pane_state_mut(internal_pane_id)
            .expect("reported pane")
            .seen = false;

        let requests = closing_block_text_adapter_requests(
            &pane_id,
            "session-current",
            "**Critical action points (0 blocking)**\n\n\
             **What to test**\n\n\
             1. Confirm the green state remains visible.\n\n\
             Done here.\n",
        );
        apply_closing_block_adapter_requests(&mut app, "informational-item", requests);

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(terminal.closing_items().len(), 1);
        assert!(!terminal.has_pending_human_input());
        let pane = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap();
        let projection = pane.agent_projection(terminal);
        assert!(!projection.open_blockers);
        assert_eq!(projection.gate_count, 0);
        let info = app.pane_info(0, internal_pane_id).unwrap();
        assert_eq!(info.agent_status, crate::api::schema::AgentStatus::Done);
        assert_eq!(info.items[0].label, "What to test");
    }

    #[test]
    fn pane_report_agent_declared_wait_and_reported_at_are_exposed_in_api_and_event() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let reported_at = "2026-08-10T10:00:00Z".to_string();
        let last_turn_at = "2026-08-10T09:59:50Z".to_string();

        let response = app.handle_pane_report_agent(
            "declared-wait".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Working,
                v: Some(2),
                message: Some("waiting for CI".into()),
                seq: Some(1),
                wait: Some("CI run 4123".into()),
                eta_s: Some(720),
                reported_at: Some(reported_at.clone()),
                last_turn_at: Some(last_turn_at.clone()),
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(Vec::new()),
                items: Some(Vec::new()),
                decisions: Some(Vec::new()),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "get".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget { pane_id }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert_eq!(pane.wait.as_deref(), Some("CI run 4123"));
        assert_eq!(pane.eta_s, Some(720));
        assert_eq!(pane.reported_at.as_deref(), Some(reported_at.as_str()));
        assert_eq!(pane.last_turn_at.as_deref(), Some(last_turn_at.as_str()));
        assert!(app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| matches!(
                &event.data,
                crate::api::schema::EventData::PaneAgentStatusChanged {
                    agent_status: crate::api::schema::AgentStatus::Working,
                    wait: Some(wait),
                    eta_s: Some(720),
                    reported_at: Some(value),
                    last_turn_at: Some(turn_at),
                    ..
                } if wait == "CI run 4123" && value == &reported_at && turn_at == &last_turn_at
            )));
    }

    #[test]
    fn stale_watchdog_is_visible_in_api_event_and_cleared_by_fresh_report() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        let started = std::time::Instant::now();
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Claude), AgentState::Working);
        terminal
            .set_hook_authority_report_at(
                "herdr:claude-closing-block".into(),
                "claude".into(),
                AgentState::Working,
                None,
                None,
                None,
                Some("2026-08-10T10:00:00Z".into()),
                None,
                Some(1),
                started,
            )
            .expect("working report accepted");

        let updates = app.state.mark_due_agent_status_stale_at(
            started + crate::terminal::state::AGENT_BUSY_STALE_SILENCE,
        );
        assert_eq!(updates.len(), 1);
        for update in &updates {
            app.emit_pane_state_update(update);
        }
        let pane = app.pane_info(0, internal_pane_id).expect("pane info");
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Stale);
        assert!(app
            .event_hub
            .events_after(0)
            .iter()
            .any(|(_, event)| matches!(
                event.data,
                crate::api::schema::EventData::PaneAgentStatusChanged {
                    agent_status: crate::api::schema::AgentStatus::Stale,
                    ..
                }
            )));

        let response = app.handle_pane_report_agent(
            "fresh".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Working,
                v: Some(2),
                message: None,
                seq: Some(2),
                wait: None,
                eta_s: None,
                reported_at: Some("2026-08-10T10:21:00Z".into()),
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(Vec::new()),
                items: Some(Vec::new()),
                decisions: Some(Vec::new()),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let pane = app.pane_info(0, internal_pane_id).expect("pane info");
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
        assert!(!app.state.terminals[&terminal_id].supervisor_stale);
    }

    #[test]
    fn stale_is_not_a_self_reportable_pane_agent_state() {
        let raw = serde_json::json!({
            "pane_id": "w1:p1",
            "source": "herdr:claude-closing-block",
            "agent": "claude",
            "state": "stale",
            "v": 2,
        });
        assert!(serde_json::from_value::<PaneReportAgentParams>(raw).is_err());
    }

    #[test]
    fn stale_v2_closing_block_report_does_not_replace_newer_gate_without_pty() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);

        let report = |text: &str, seq: u64| PaneReportAgentParams {
            pane_id: pane_id.clone(),
            source: "herdr:claude-closing-block".into(),
            agent: "claude".into(),
            state: crate::api::schema::PaneAgentState::Blocked,
            v: Some(2),
            message: Some(text.into()),
            seq: Some(seq),
            wait: None,
            eta_s: None,
            reported_at: None,
            agent_session_id: None,
            agent_session_path: None,
            gates: Some(vec![crate::api::schema::ClosingBlockItem {
                blocking: true,
                n: 1,
                label: "Gate".into(),
                text: text.into(),
                pr: None,
                ticket: None,
                url: None,
                default: None,
                default_at: None,
            }]),
            items: Some(Vec::new()),
            decisions: Some(Vec::new()),
            completion: Some(crate::api::schema::ClosingCompletion::Incomplete),
            external_wait: None,
            parse_status: Some(crate::api::schema::ClosingParseStatus::Ok),
            workers_unknown: Some(false),
            agents: None,
            last_turn_at: None,
            settle_ready: None,
        };

        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("newer".into(), report("newer gate", 10)),
        )
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent("stale".into(), report("stale gate", 9)),
        )
        .unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "get".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget { pane_id }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert_eq!(pane.gates[0].text, "newer gate");
    }

    #[test]
    fn older_closing_metadata_cannot_overwrite_a_newer_typed_task_report() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        let mut completed = closing_block_report(&pane_id, 20, Vec::new());
        completed.completion = Some(crate::api::schema::ClosingCompletion::Complete);
        completed.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        completed.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("new-complete".into(), completed))
                .unwrap();

        let mut older = metadata_params(pane_id.clone());
        older.title = None;
        older.source = "herdr:codex-closing-block".into();
        older.applies_to_source = Some("herdr:codex-closing-block".into());
        older.seq = Some(10);
        older.tokens = std::collections::HashMap::from([
            ("closing_blocking".into(), Some("1".into())),
            ("closing_gates".into(), Some("old gate".into())),
            ("closing_completion".into(), Some("incomplete".into())),
            ("closing_parse".into(), Some("ok".into())),
        ]);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("old-metadata".into(), older))
                .unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_completion")
                .map(String::as_str),
            Some("complete")
        );
        assert!(terminal.closing_gates().is_empty());
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Done
        );
    }

    #[test]
    fn old_session_task_reports_cannot_mutate_the_current_session() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id = bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:codex",
            "codex",
            "session-current",
        );

        let mut current = closing_block_report(&pane_id, 20, Vec::new());
        current.agent_session_id = Some("session-current".into());
        current.completion = Some(crate::api::schema::ClosingCompletion::Complete);
        current.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        current.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("current-session".into(), current))
                .unwrap();

        let mut old = closing_block_report(&pane_id, 21, vec![test_gate()]);
        old.agent_session_id = Some("session-old".into());
        old.state = crate::api::schema::PaneAgentState::Blocked;
        old.completion = Some(crate::api::schema::ClosingCompletion::Incomplete);
        old.parse_status = Some(crate::api::schema::ClosingParseStatus::Ok);
        old.workers_unknown = Some(false);
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("old-session".into(), old)).unwrap();

        let mut old_metadata = metadata_params(pane_id);
        old_metadata.title = None;
        old_metadata.source = "herdr:codex-closing-block".into();
        old_metadata.agent = Some("codex".into());
        old_metadata.applies_to_source = Some("herdr:codex-closing-block".into());
        old_metadata.agent_session_id = Some("session-old".into());
        old_metadata.seq = Some(21);
        old_metadata.tokens = std::collections::HashMap::from([
            ("closing_blocking".into(), Some("1".into())),
            ("closing_completion".into(), Some("incomplete".into())),
            ("closing_parse".into(), Some("ok".into())),
        ]);
        let response = app.handle_pane_report_metadata("old-session-metadata".into(), old_metadata);
        assert_eq!(metadata_error_code(&response), "agent_session_mismatch");

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.closing_gates().is_empty());
        assert_eq!(
            terminal
                .metadata_tokens_for_api()
                .get("closing_completion")
                .map(String::as_str),
            Some("complete")
        );
    }

    #[test]
    fn legacy_v1_report_skips_closing_block_arrays_without_pty() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);

        let response = app.handle_pane_report_agent(
            "legacy".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: Some(1),
                message: Some("legacy gate".into()),
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(vec![crate::api::schema::ClosingBlockItem {
                    blocking: true,
                    n: 1,
                    label: "Gate".into(),
                    text: "legacy gate".into(),
                    pr: None,
                    ticket: None,
                    url: None,
                    default: Some("approve".into()),
                    default_at: Some("2026-08-09T12:00:00Z".into()),
                }]),
                items: Some(Vec::new()),
                decisions: Some(Vec::new()),
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "get".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget { pane_id }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert!(pane.gates.is_empty());
    }

    #[test]
    fn foreign_version_report_is_a_silent_no_op_without_pty() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);

        let response = app.handle_pane_report_agent(
            "foreign-version".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: Some(1),
                message: Some("a v1 gate".into()),
                seq: Some(1),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: None,
                items: None,
                decisions: None,
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        // Even a missing pane answers success: the skip happens before any
        // lookup, so a foreign version can never surface an error.
        let response = app.handle_pane_report_agent(
            "foreign-version-missing-pane".into(),
            PaneReportAgentParams {
                pane_id: "w9:p9".into(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                state: crate::api::schema::PaneAgentState::Blocked,
                v: Some(3),
                message: None,
                seq: Some(2),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: None,
                items: None,
                decisions: None,
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let response = app.handle_api_request(crate::api::schema::Request {
            id: "get".into(),
            method: crate::api::schema::Method::PaneGet(PaneTarget { pane_id }),
        });
        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        let ResponseResult::PaneInfo { pane } = success.result else {
            panic!("expected pane info");
        };
        assert!(pane.gates.is_empty());
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Working);
    }

    #[test]
    fn foreign_version_with_reshaped_state_is_a_silent_no_op() {
        let (mut app, _) = app_with_test_workspace();
        let request = serde_json::from_value::<crate::api::schema::Request>(serde_json::json!({
            "id": "foreign-reshaped-state",
            "method": "pane.report_agent",
            "params": {
                "pane_id": "w9:p9",
                "source": "herdr:claude-closing-block",
                "agent": "claude",
                "state": {"phase": "paused"},
                "v": 3,
                "wait": {"until": "ci"}
            }
        }))
        .expect("foreign versions must parse before the handler skips them");

        let response = app.handle_api_request(request);
        let success: crate::api::schema::SuccessResponse =
            serde_json::from_str(&response).expect("foreign versions must return success");
        assert_eq!(success.result, ResponseResult::Ok {});
    }

    #[test]
    fn pane_tokens_are_independent_from_presentation_guards() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);
        let mut params = metadata_params(pane_id);
        params.title = None;
        params.agent = Some("codex".into());
        params.tokens =
            std::collections::HashMap::from([("summary".into(), Some("global".into()))]);

        let response = app.handle_pane_report_metadata("guarded".into(), params);

        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id].metadata_tokens.values(),
            std::collections::HashMap::from([("summary".into(), "global".into())])
        );
    }

    #[test]
    fn pane_metadata_uses_one_sequence_for_presentation_and_tokens() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut presentation = metadata_params(pane_id.clone());
        presentation.seq = Some(10);
        let response = app.handle_pane_report_metadata("presentation".into(), presentation);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut stale_token = metadata_params(pane_id.clone());
        stale_token.title = None;
        stale_token.tokens =
            std::collections::HashMap::from([("summary".into(), Some("stale".into()))]);
        stale_token.seq = Some(9);
        let response = app.handle_pane_report_metadata("stale".into(), stale_token);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        assert!(app.state.terminals[&terminal_id]
            .metadata_tokens
            .values()
            .is_empty());
    }

    #[test]
    fn pane_metadata_ignored_after_process_exit_does_not_poison_sequence() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Pi), AgentState::Idle);

        let mut initial = metadata_params(pane_id.clone());
        initial.source = "custom:pi-metadata".into();
        initial.agent = Some("pi".into());
        initial.seq = Some(100);
        let response = app.handle_pane_report_metadata("initial".into(), initial);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut initial_tokens = metadata_params(pane_id.clone());
        initial_tokens.source = "custom:pi-tokens".into();
        initial_tokens.agent = Some("pi".into());
        initial_tokens.title = None;
        initial_tokens.tokens =
            std::collections::HashMap::from([("generation".into(), Some("old".into()))]);
        initial_tokens.seq = Some(100);
        let response = app.handle_pane_report_metadata("initial-tokens".into(), initial_tokens);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let exit_at = std::time::Instant::now() + std::time::Duration::from_millis(1);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                false,
                false,
                false,
                true,
                exit_at,
            );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                None,
                AgentState::Unknown,
                false,
                false,
                false,
                false,
                false,
                exit_at + std::time::Duration::from_millis(1),
            );

        let mut stale = metadata_params(pane_id.clone());
        stale.source = "custom:pi-metadata".into();
        stale.agent = Some("pi".into());
        stale.title = Some("stale".into());
        stale.seq = Some(200);
        let response = app.handle_pane_report_metadata("stale".into(), stale);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut official = metadata_params(pane_id.clone());
        official.source = "herdr:pi".into();
        official.seq = Some(200);
        let response = app.handle_pane_report_metadata("official".into(), official);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert!(terminal.metadata_report_sequence_is_fresh("custom:pi-metadata", Some(1)));
        assert!(terminal.metadata_report_sequence_is_fresh("custom:pi-tokens", Some(1)));
        assert!(terminal.metadata_report_sequence_is_fresh("herdr:pi", Some(1)));

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_screen_signals_at(
                Some(Agent::Pi),
                AgentState::Idle,
                false,
                false,
                false,
                false,
                false,
                exit_at + std::time::Duration::from_millis(2),
            );
        let mut fresh = metadata_params(pane_id.clone());
        fresh.source = "custom:pi-metadata".into();
        fresh.agent = Some("pi".into());
        fresh.title = Some("fresh".into());
        fresh.seq = Some(1);
        let response = app.handle_pane_report_metadata("fresh".into(), fresh);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut fresh_tokens = metadata_params(pane_id);
        fresh_tokens.source = "custom:pi-tokens".into();
        fresh_tokens.agent = Some("pi".into());
        fresh_tokens.title = None;
        fresh_tokens.tokens =
            std::collections::HashMap::from([("generation".into(), Some("new".into()))]);
        fresh_tokens.seq = Some(1);
        let response = app.handle_pane_report_metadata("fresh-tokens".into(), fresh_tokens);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            terminal.agent_metadata["custom:pi-metadata"]
                .title
                .as_deref(),
            Some("fresh")
        );
        assert_eq!(
            terminal
                .metadata_tokens
                .values()
                .get("generation")
                .map(String::as_str),
            Some("new")
        );
    }

    #[test]
    fn pane_report_metadata_accepts_documented_source_chars_and_max_ttl() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.ttl_ms = Some(METADATA_TTL_MAX_MS);

        let response = app.handle_pane_report_metadata("req".into(), params);

        let success: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(success.id, "req");
    }

    #[test]
    fn pane_report_metadata_rejects_invalid_source_shape() {
        let (mut app, pane_id) = app_with_test_workspace();
        for source in ["", "user metadata", "user/metadata", "user:\u{7f}metadata"] {
            let mut params = metadata_params(pane_id.clone());
            params.source = source.into();

            let response = app.handle_pane_report_metadata("req".into(), params);

            assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
        }
    }

    #[test]
    fn pane_report_metadata_rejects_long_source() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.source = "a".repeat(METADATA_SOURCE_MAX_CHARS + 1);

        let response = app.handle_pane_report_metadata("req".into(), params);

        assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
    }

    #[test]
    fn pane_report_metadata_rejects_invalid_applies_to_source() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut params = metadata_params(pane_id);
        params.applies_to_source = Some("herdr source".into());

        let response = app.handle_pane_report_metadata("req".into(), params);

        assert_eq!(metadata_error_code(&response), "invalid_metadata_source");
    }

    #[test]
    fn pane_report_metadata_validates_session_guard_shape() {
        let (mut app, pane_id) = app_with_test_workspace();
        let mut blank = metadata_params(pane_id.clone());
        blank.agent = Some("codex".into());
        blank.applies_to_source = Some("herdr:codex".into());
        blank.agent_session_id = Some(" \n ".into());
        let response = app.handle_pane_report_metadata("blank".into(), blank);
        assert_eq!(metadata_error_code(&response), "invalid_agent_session");

        let mut unbound = metadata_params(pane_id);
        unbound.agent_session_id = Some("session-1".into());
        let response = app.handle_pane_report_metadata("unbound".into(), unbound);
        assert_eq!(metadata_error_code(&response), "invalid_metadata_request");
    }

    #[test]
    fn pane_report_metadata_rejects_ttl_outside_supported_range() {
        let (mut app, pane_id) = app_with_test_workspace();
        for ttl_ms in [0, METADATA_TTL_MAX_MS + 1] {
            let mut params = metadata_params(pane_id.clone());
            params.ttl_ms = Some(ttl_ms);

            let response = app.handle_pane_report_metadata("req".into(), params);

            assert_eq!(metadata_error_code(&response), "invalid_metadata_ttl");
        }
    }

    fn guarded_session_name_params(
        pane_id: String,
        agent: &str,
        lifecycle_source: &str,
        session_id: &str,
        session_name: &str,
        seq: u64,
    ) -> PaneReportMetadataParams {
        let mut params = metadata_params(pane_id);
        params.source = crate::work_title::SESSION_NAME_SOURCE.into();
        params.agent = Some(agent.into());
        params.applies_to_source = Some(lifecycle_source.into());
        params.agent_session_id = Some(session_id.into());
        params.title = None;
        params.seq = Some(seq);
        params.work_context = Some(crate::work_context::PaneWorkContext {
            session_name: Some(session_name.into()),
            ..Default::default()
        });
        params
    }

    #[test]
    fn guarded_session_names_land_rename_after_rename_and_keep_turn_context() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:claude", "claude", "session-1");

        // A normal turn first establishes prompt-derived work context.
        let mut turn = guarded_work_title_params(
            pane_id.clone(),
            "claude",
            "herdr:claude",
            "session-1",
            "Fix MAT-12 billing",
            1,
        );
        turn.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-12".into()],
            work_title: Some("Fix MAT-12 billing".into()),
            ..Default::default()
        });
        let response = app.handle_pane_report_metadata("turn".into(), turn);
        assert!(
            serde_json::from_str::<SuccessResponse>(&response).is_ok(),
            "{response}"
        );

        for (seq, name) in [(2, "First session name"), (3, "Renamed session")] {
            let params = guarded_session_name_params(
                pane_id.clone(),
                "claude",
                "herdr:claude",
                "session-1",
                name,
                seq,
            );
            let response = app.handle_pane_report_metadata(format!("name{seq}"), params);
            assert!(
                serde_json::from_str::<SuccessResponse>(&response).is_ok(),
                "{response}"
            );
            let context = app.state.terminals[&terminal_id].effective_work_context();
            assert_eq!(context.session_name.as_deref(), Some(name));
            // The rename must not erase what the last turn established.
            assert_eq!(context.ticket_ids, vec!["MAT-12"]);
            assert_eq!(context.work_title.as_deref(), Some("Fix MAT-12 billing"));
        }
    }

    #[test]
    fn guarded_session_names_reject_unguarded_and_overreaching_reports() {
        let (mut app, pane_id) = app_with_test_workspace();
        bind_test_agent_session(&mut app, &pane_id, "herdr:claude", "claude", "session-1");

        // A session name with no session guard is not a rename anyone may make.
        let mut unguarded = guarded_session_name_params(
            pane_id.clone(),
            "claude",
            "herdr:claude",
            "session-1",
            "Sneaky",
            1,
        );
        unguarded.agent_session_id = None;
        unguarded.agent = None;
        unguarded.applies_to_source = None;
        let response = app.handle_pane_report_metadata("unguarded".into(), unguarded);
        assert_eq!(metadata_error_code(&response), "invalid_work_context");

        // A rename may carry the name and nothing else.
        let mut overreaching = guarded_session_name_params(
            pane_id.clone(),
            "claude",
            "herdr:claude",
            "session-1",
            "Sneaky",
            2,
        );
        overreaching.work_context = Some(crate::work_context::PaneWorkContext {
            session_name: Some("Sneaky".into()),
            ticket_ids: vec!["MAT-99".into()],
            ..Default::default()
        });
        let response = app.handle_pane_report_metadata("overreaching".into(), overreaching);
        assert_eq!(metadata_error_code(&response), "invalid_work_context");

        // A rename aimed at a session this pane is not running is rejected.
        let stale = guarded_session_name_params(
            pane_id.clone(),
            "claude",
            "herdr:claude",
            "session-other",
            "Sneaky session",
            3,
        );
        let response = app.handle_pane_report_metadata("stale".into(), stale);
        assert_eq!(metadata_error_code(&response), "agent_session_mismatch");

        let single_token = guarded_session_name_params(
            pane_id,
            "claude",
            "herdr:claude",
            "session-1",
            "random-id",
            4,
        );
        let response = app.handle_pane_report_metadata("single-token".into(), single_token);
        assert_eq!(metadata_error_code(&response), "invalid_work_context");
    }

    #[test]
    fn guarded_work_titles_skip_duplicates_and_reject_stale_sessions() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:codex", "codex", "session-new");

        let first = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-new",
            "Fix Billing Retry Regression",
            20,
        );
        let response = app.handle_pane_report_metadata("first".into(), first);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        let revision = app.state.terminals[&terminal_id].revision;
        let reported_at = app.state.terminals[&terminal_id].agent_metadata
            [crate::work_title::WORK_TITLE_SOURCE]
            .reported_at;

        for seq in 21..=40 {
            let duplicate = guarded_work_title_params(
                pane_id.clone(),
                "codex",
                "herdr:codex",
                "session-new",
                "Fix Billing Retry Regression",
                seq,
            );
            let response = app.handle_pane_report_metadata(format!("duplicate-{seq}"), duplicate);
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        }
        assert_eq!(app.state.terminals[&terminal_id].revision, revision);
        assert_eq!(
            app.state.terminals[&terminal_id].agent_metadata[crate::work_title::WORK_TITLE_SOURCE]
                .reported_at,
            reported_at
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.event_hub
                .events_after(0)
                .iter()
                .filter(|(_, event)| event.event == EventKind::TabRenamed)
                .count(),
            0
        );

        let stale = guarded_work_title_params(
            pane_id,
            "codex",
            "herdr:codex",
            "session-old",
            "Overwrite Newer Work Title",
            19,
        );
        let response = app.handle_pane_report_metadata("stale".into(), stale);
        assert_eq!(metadata_error_code(&response), "agent_session_mismatch");
        assert_eq!(
            app.state.terminals[&terminal_id].agent_metadata[crate::work_title::WORK_TITLE_SOURCE]
                .title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert!(
            !app.state.terminals[&terminal_id]
                .metadata_report_sequence_is_fresh(crate::work_title::WORK_TITLE_SOURCE, Some(26),),
            "the terse turn still refreshes guarded metadata sequencing"
        );
    }

    #[test]
    fn guarded_work_titles_keep_followups_and_retitle_for_new_work() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:codex", "codex", "session-new");

        let mut first = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-new",
            "Fix MAT-1549 Vault Retention",
            20,
        );
        first.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-1549".into()],
            pr_urls: vec!["https://github.com/o/r/pull/1549".into()],
            work_title: first.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("first".into(), first)).unwrap();

        app.state.workspaces[0].tabs[0].set_user_custom_name("Human tab label".into());

        let mut follow_up = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-new",
            "Can We Fix the Fact That Windows Have No Stable Task Title",
            21,
        );
        follow_up.work_context = Some(crate::work_context::PaneWorkContext {
            work_title: follow_up.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("follow-up".into(), follow_up))
                .unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Fix MAT-1549 Vault Retention")
        );

        let mut new_ticket = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-new",
            "Review SCA-42 Sidebar Labels",
            22,
        );
        new_ticket.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["SCA-42".into()],
            work_title: new_ticket.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("new-ticket".into(), new_ticket))
                .unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Review SCA-42 Sidebar Labels")
        );

        let mut long_brief = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-new",
            "Rebuild the Audit Event Index",
            23,
        );
        long_brief.work_title_long_brief = true;
        long_brief.work_context = Some(crate::work_context::PaneWorkContext {
            work_title: long_brief.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("long-brief".into(), long_brief))
                .unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Rebuild the Audit Event Index")
        );

        let mut one_a =
            guarded_work_title_params(pane_id, "codex", "herdr:codex", "session-new", "1A", 24);
        one_a.work_context = Some(crate::work_context::PaneWorkContext {
            work_title: one_a.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("one-a".into(), one_a)).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Rebuild the Audit Event Index")
        );
        assert_eq!(
            app.state.workspaces[0].tabs[0].custom_name.as_deref(),
            Some("Human tab label")
        );
    }

    #[test]
    fn ac1_ac4_ac5_ac6_guarded_turns_replace_hook_context_with_manual_precedence() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:codex", "codex", "session-1");
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: None,
                branch: Some("feat/SCA-88-context".into()),
                ..Default::default()
            })
            .unwrap();

        let mut first = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-1",
            "Implement MAT-1 context",
            20,
        );
        first.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-1".into()],
            pr_urls: vec!["https://github.com/o/r/pull/1".into()],
            preview_urls: vec!["https://first.vercel.app".into()],
            work_title: first.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("first".into(), first)).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .ticket_ids,
            vec!["MAT-1"]
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .preview_urls,
            vec!["https://first.vercel.app"]
        );
        let pane_updates_before = app
            .event_hub
            .events_after(0)
            .iter()
            .filter(|(_, event)| event.event == EventKind::PaneUpdated)
            .count();

        let mut same_title_new_pr = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-1",
            "Implement MAT-1 context",
            21,
        );
        same_title_new_pr.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-1".into()],
            pr_urls: vec!["https://github.com/o/r/pull/9".into()],
            preview_urls: vec!["https://same-title.vercel.app".into()],
            work_title: same_title_new_pr.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("same-title-new-pr".into(), same_title_new_pr),
        )
        .unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .pr_urls,
            vec!["https://github.com/o/r/pull/9"]
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .preview_urls,
            vec!["https://same-title.vercel.app"]
        );
        assert_eq!(
            app.event_hub
                .events_after(0)
                .iter()
                .filter(|(_, event)| event.event == EventKind::PaneUpdated)
                .count(),
            pane_updates_before + 1,
            "a context-only turn mutation emits exactly one pane update"
        );

        let mut second = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-1",
            "Continue MAT-2 context",
            22,
        );
        second.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-2".into()],
            pr_urls: vec!["https://github.com/o/r/pull/2".into()],
            preview_urls: vec!["https://second.vercel.app".into()],
            work_title: second.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("second".into(), second))
                .unwrap();
        let context = app.state.terminals[&terminal_id].effective_work_context();
        assert_eq!(context.ticket_ids, vec!["MAT-2"]);
        assert_eq!(context.pr_urls, vec!["https://github.com/o/r/pull/2"]);
        assert_eq!(context.preview_urls, vec!["https://second.vercel.app"]);
        assert_eq!(
            context.work_title.as_deref(),
            Some("Continue MAT-2 context")
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: None,
                ticket_ids: Some(vec!["MAT-500".into()]),
                pr_urls: Some(vec!["https://github.com/manual/repo/pull/99".into()]),
                work_title: Some("Manual context".into()),
                ..Default::default()
            })
            .unwrap();
        let mut third = guarded_work_title_params(
            pane_id.clone(),
            "codex",
            "herdr:codex",
            "session-1",
            "Ship MAT-3 context",
            23,
        );
        third.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-3".into()],
            pr_urls: vec!["https://github.com/o/r/pull/3".into()],
            preview_urls: vec!["https://third.vercel.app".into()],
            work_title: third.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("third".into(), third)).unwrap();

        let mut stale = guarded_work_title_params(
            pane_id,
            "codex",
            "herdr:codex",
            "session-1",
            "Overwrite With MAT-777",
            22,
        );
        stale.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-777".into()],
            pr_urls: vec!["https://github.com/o/r/pull/777".into()],
            work_title: stale.title.clone(),
            ..Default::default()
        });
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("stale".into(), stale)).unwrap();

        let context = app.state.terminals[&terminal_id].effective_work_context();
        // The manual declaration owns the link fields outright, so the hook's
        // ticket and pull request drop out of the effective context while the
        // fields it does not declare still come from the hook.
        assert_eq!(context.ticket_ids, vec!["MAT-500"]);
        assert_eq!(
            context.pr_urls,
            vec!["https://github.com/manual/repo/pull/99"]
        );
        assert_eq!(context.preview_urls, vec!["https://third.vercel.app"]);
        assert_eq!(context.work_title.as_deref(), Some("Manual context"));
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.event_hub
                .events_after(0)
                .iter()
                .filter(|(_, event)| event.event == EventKind::TabRenamed)
                .count(),
            0
        );
    }

    fn seed_manual_work_context(app: &mut App, terminal_id: &crate::terminal::TerminalId) {
        app.state
            .terminals
            .get_mut(terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                repo: None,
                ticket_ids: Some(vec!["MAT-500".into()]),
                pr_urls: Some(vec!["https://github.com/manual/repo/pull/99".into()]),
                work_title: Some("Manual context".into()),
                ..Default::default()
            })
            .unwrap();
    }

    fn populate_guarded_hook_context(
        app: &mut App,
        pane_id: &str,
        agent: &str,
        lifecycle_source: &str,
        session_id: &str,
        seq: u64,
    ) {
        let mut params = guarded_work_title_params(
            pane_id.to_string(),
            agent,
            lifecycle_source,
            session_id,
            "Implement MAT-1 context",
            seq,
        );
        params.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-1".into()],
            pr_urls: vec!["https://github.com/o/r/pull/1".into()],
            preview_urls: vec!["https://hook.vercel.app".into()],
            work_title: params.title.clone(),
            ..Default::default()
        });
        let response = app.handle_pane_report_metadata("populate".into(), params);
        let _: SuccessResponse =
            serde_json::from_str(&response).unwrap_or_else(|_| panic!("{response}"));
    }

    fn assert_only_manual_work_context_remains(
        app: &App,
        terminal_id: &crate::terminal::TerminalId,
    ) {
        let context = app.state.terminals[terminal_id].effective_work_context();
        assert_eq!(context.ticket_ids, vec!["MAT-500"]);
        assert_eq!(
            context.pr_urls,
            vec!["https://github.com/manual/repo/pull/99"]
        );
        assert!(context.preview_urls.is_empty());
        assert_eq!(context.work_title.as_deref(), Some("Manual context"));
    }

    fn assert_hook_and_manual_work_context_live(
        app: &App,
        terminal_id: &crate::terminal::TerminalId,
    ) {
        let context = app.state.terminals[terminal_id].effective_work_context();
        // The hook tier is live here, which the preview URL is what proves:
        // the link fields stay the manual declaration's because a declaration
        // replaces the tiers below it.
        assert_eq!(context.ticket_ids, vec!["MAT-500"]);
        assert_eq!(context.preview_urls, vec!["https://hook.vercel.app"]);
        assert_eq!(
            context.pr_urls,
            vec!["https://github.com/manual/repo/pull/99"]
        );
    }

    #[cfg(unix)]
    fn save_and_restore_work_context(app: &mut App) -> crate::work_context::PaneWorkContext {
        let mut env = crate::config::TestConfigEnvGuard::acquire();
        let config_home = std::env::temp_dir().join(format!(
            "herdr-pr24-work-context-save-{}",
            crate::config::test_unique_suffix()
        ));
        env.set("XDG_CONFIG_HOME", &config_home);
        env.remove(crate::session::SESSION_ENV_VAR);
        app.session_writer = std::sync::Arc::new(std::sync::Mutex::new(
            crate::persist::SessionWriter::new(false),
        ));

        app.save_session_now();
        assert!(!app.state.session_dirty, "session save should complete");
        let snapshot = crate::persist::load().expect("session save should write a snapshot");
        let (events, _event_rx) = tokio::sync::mpsc::channel(4);
        let (workspaces, terminals, runtimes) = crate::persist::restore(
            &snapshot,
            None,
            24,
            80,
            0,
            "/bin/sh",
            app.state.shell_mode,
            false,
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        );
        let root_pane = workspaces[0].tabs[0].root_pane;
        let terminal_id = workspaces[0].tabs[0].panes[&root_pane]
            .attached_terminal_id
            .clone();
        let context = terminals[&terminal_id].effective_work_context().clone();
        for runtime in runtimes.into_values() {
            runtime.shutdown();
        }

        drop(env);
        let _ = std::fs::remove_dir_all(config_home);
        context
    }

    #[test]
    fn agent_release_clears_hook_work_context_and_keeps_manual_tier() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "custom:pi", "pi", "session-pi");
        seed_manual_work_context(&mut app, &terminal_id);
        populate_guarded_hook_context(&mut app, &pane_id, "pi", "custom:pi", "session-pi", 20);
        assert_hook_and_manual_work_context_live(&app, &terminal_id);

        let response = app.handle_pane_release_agent(
            "release".into(),
            PaneReleaseAgentParams {
                pane_id,
                source: "custom:pi".into(),
                agent: "pi".into(),
                seq: Some(30),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_only_manual_work_context_remains(&app, &terminal_id);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn accepted_hook_work_context_is_saved_and_restored() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.no_session = false;
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:codex", "codex", "session-codex");

        populate_guarded_hook_context(
            &mut app,
            &pane_id,
            "codex",
            "herdr:codex",
            "session-codex",
            20,
        );

        assert!(app.state.session_dirty);
        assert!(app.session_save_deadline.is_some());
        assert_eq!(pane_updated_events(&app), 1);
        let restored = save_and_restore_work_context(&mut app);
        assert_eq!(restored.ticket_ids, vec!["MAT-1"]);
        assert_eq!(restored.pr_urls, vec!["https://github.com/o/r/pull/1"]);
        assert_eq!(restored.preview_urls, vec!["https://hook.vercel.app"]);
        assert_eq!(
            restored.work_title.as_deref(),
            Some("Implement MAT-1 context")
        );
        assert_eq!(
            app.state.terminals[&terminal_id].effective_work_context(),
            &crate::work_context::PaneWorkContext {
                ticket_ids: vec!["MAT-1".into()],
                pr_urls: vec!["https://github.com/o/r/pull/1".into()],
                preview_urls: vec!["https://hook.vercel.app".into()],
                work_title: Some("Implement MAT-1 context".into()),
                ..Default::default()
            }
        );
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn release_with_foreign_session_saves_hook_clear_and_restores_without_it() {
        let (mut app, pane_id) = app_with_test_workspace();
        app.no_session = false;
        let terminal_id = bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:claude",
            "claude",
            "claude-session",
        );
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(Some(Agent::Pi), AgentState::Idle);
        terminal
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["MAT-1".into()],
                pr_urls: vec!["https://github.com/o/r/pull/1".into()],
                preview_urls: vec!["https://stale.vercel.app".into()],
                work_title: Some("Stale hook context".into()),
                ..Default::default()
            })
            .unwrap();

        let response = app.handle_pane_release_agent(
            "release-foreign".into(),
            PaneReleaseAgentParams {
                pane_id,
                source: "custom:pi".into(),
                agent: "pi".into(),
                seq: Some(21),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id].effective_work_context(),
            &crate::work_context::PaneWorkContext::default()
        );
        assert!(app.state.session_dirty);
        assert!(app.session_save_deadline.is_some());
        assert_eq!(pane_updated_events(&app), 1);

        let restored = save_and_restore_work_context(&mut app);
        assert_eq!(restored, crate::work_context::PaneWorkContext::default());
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn deferred_resume_failure_saves_hook_clear_and_restores_without_it() {
        let mut app = {
            let (_api_tx, api_rx) = tokio::sync::mpsc::unbounded_channel();
            App::new(
                &Config::default(),
                true,
                None,
                api_rx,
                crate::api::EventHub::default(),
            )
        };
        let workspace = Workspace::test_new("resume-failure");
        let pane_id = workspace.tabs[0].root_pane;
        let terminal_id = workspace.terminal_id(pane_id).cloned().unwrap();
        app.state.view.pane_infos = workspace.tabs[0]
            .layout
            .panes(ratatui::layout::Rect::new(0, 0, 100, 30));
        app.state.view.terminal_area = ratatui::layout::Rect::new(0, 0, 100, 30);
        app.state.workspaces = vec![workspace];
        app.state.active = Some(0);
        app.state.ensure_test_terminals();
        app.state.default_shell = "/herdr/pr24/missing-shell".into();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .replace_hook_work_context(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["MAT-1".into()],
                pr_urls: vec!["https://github.com/o/r/pull/1".into()],
                work_title: Some("Stale hook context".into()),
                ..Default::default()
            })
            .unwrap();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .pending_agent_resume_plan = Some(crate::agent_resume::AgentResumePlan {
            agent: "codex".into(),
            argv: vec!["codex".into()],
            dedupe_key: "herdr:codex\0codex\0Id\0resume-failure".into(),
        });
        app.no_session = false;

        assert!(!app.start_pending_agent_resumes(true));
        assert_eq!(
            app.state.terminals[&terminal_id].effective_work_context(),
            &crate::work_context::PaneWorkContext::default()
        );
        assert!(app.state.session_dirty);
        assert!(app.session_save_deadline.is_some());
        assert_eq!(pane_updated_events(&app), 1);

        let restored = save_and_restore_work_context(&mut app);
        assert_eq!(restored, crate::work_context::PaneWorkContext::default());
    }

    #[test]
    fn hook_authority_clear_clears_hook_work_context_and_keeps_manual_tier() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:pi", "pi", "session-pi");
        seed_manual_work_context(&mut app, &terminal_id);
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Pi),
                crate::detect::AgentState::Idle,
            );
        let response = app.handle_pane_report_agent(
            "working".into(),
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:pi".into(),
                agent: "pi".into(),
                state: crate::api::schema::PaneAgentState::Working,
                v: None,
                message: None,
                seq: Some(5),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: Some("session-pi".into()),
                agent_session_path: None,
                gates: None,
                items: None,
                decisions: None,
                completion: None,
                external_wait: None,
                parse_status: None,
                workers_unknown: None,
                agents: None,
                last_turn_at: None,
                settle_ready: None,
            },
        );
        let _: SuccessResponse =
            serde_json::from_str(&response).unwrap_or_else(|_| panic!("{response}"));
        populate_guarded_hook_context(&mut app, &pane_id, "pi", "herdr:pi", "session-pi", 20);
        assert_hook_and_manual_work_context_live(&app, &terminal_id);

        let response = app.handle_pane_clear_agent_authority(
            "clear".into(),
            PaneClearAgentAuthorityParams {
                pane_id,
                source: Some("herdr:pi".into()),
                seq: Some(6),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_only_manual_work_context_remains(&app, &terminal_id);
    }

    #[test]
    fn replaced_agent_session_exposes_no_prior_refs_before_first_guarded_prompt() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        seed_manual_work_context(&mut app, &terminal_id);
        let response = app.handle_pane_report_agent_session(
            "session-1".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(10),
                agent_session_id: Some("session-1".into()),
                agent_session_path: None,
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        populate_guarded_hook_context(&mut app, &pane_id, "codex", "herdr:codex", "session-1", 20);
        assert_hook_and_manual_work_context_live(&app, &terminal_id);

        let response = app.handle_pane_report_agent_session(
            "session-2".into(),
            PaneReportAgentSessionParams {
                pane_id,
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(11),
                agent_session_id: Some("session-2".into()),
                agent_session_path: None,
                session_start_source: Some("clear".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert!(
            app.state.terminals[&terminal_id].agent_session_matches(
                "herdr:codex",
                "codex",
                "session-2"
            ),
            "the replacement session owns the terminal before its first guarded prompt"
        );

        assert_only_manual_work_context_remains(&app, &terminal_id);
    }

    #[test]
    fn stale_pane_agent_reports_route_to_the_unique_session_owner() {
        let (mut app, _) = app_with_test_workspace();
        let owner_pane_id = add_test_workspace(&mut app, "session-owner");
        let terminal_id = bind_test_agent_session(
            &mut app,
            &owner_pane_id,
            "herdr:claude",
            "claude",
            "claude-session-owner",
        );
        let owner_internal_id = app.parse_pane_id(&owner_pane_id).unwrap().1;
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);

        let session_response = app.handle_pane_report_agent_session(
            "stale-session-report".into(),
            PaneReportAgentSessionParams {
                pane_id: "missing-workspace:p15".into(),
                source: "herdr:claude-closing-block".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("claude-session-owner".into()),
                agent_session_path: None,
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&session_response)
            .unwrap_or_else(|_| panic!("session report was not routed: {session_response}"));

        let mut report = closing_block_report("missing-workspace:p15", 2, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("claude-session-owner".into());
        let response = app.handle_pane_report_agent("stale-final-report".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response)
            .unwrap_or_else(|_| panic!("agent report was not routed: {response}"));

        let pane = app.pane_info(1, owner_internal_id).unwrap();
        assert_eq!(pane.agent_status, crate::api::schema::AgentStatus::Done);
        assert_eq!(
            pane.tokens.get("closing_completion").map(String::as_str),
            Some("complete")
        );
        assert_eq!(
            app.state.terminals[&terminal_id].raw_agent_state(),
            AgentState::Idle
        );
    }

    #[test]
    fn resumed_report_agent_session_stays_on_addressed_pane_with_persisted_duplicate() {
        let (mut app, old_pane_id) = app_with_test_workspace();
        let (old_ws_idx, old_internal_pane_id) = app.parse_pane_id(&old_pane_id).unwrap();
        let old_terminal_id = app.state.workspaces[old_ws_idx]
            .pane_state(old_internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        bind_test_agent_session(
            &mut app,
            &old_pane_id,
            "herdr:claude",
            "claude",
            "resumed-session",
        );
        let old_state = app.state.terminals[&old_terminal_id].raw_agent_state();

        let resumed_pane_id = add_test_workspace(&mut app, "resumed-session");
        let (resumed_ws_idx, resumed_internal_pane_id) =
            app.parse_pane_id(&resumed_pane_id).unwrap();
        let resumed_terminal_id = app.state.workspaces[resumed_ws_idx]
            .pane_state(resumed_internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&resumed_terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Working);

        let session_response = app.handle_pane_report_agent_session(
            "resumed-session-report".into(),
            PaneReportAgentSessionParams {
                pane_id: resumed_pane_id.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("resumed-session".into()),
                agent_session_path: None,
                session_start_source: Some("resume".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&session_response).unwrap();
        assert_eq!(
            app.state.terminals[&resumed_terminal_id].current_agent_session_id(),
            Some("resumed-session")
        );
        assert_eq!(
            app.state.terminals[&old_terminal_id]
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("resumed-session")
        );
        assert!(app.state.terminals[&old_terminal_id]
            .hook_authority
            .is_none());

        let mut report = closing_block_report(&resumed_pane_id, 2, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("resumed-session".into());
        let response = app.handle_pane_report_agent("resumed-final-report".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&old_terminal_id].raw_agent_state(),
            old_state,
            "the pane carrying only persisted identity stays untouched"
        );
        assert_eq!(
            app.pane_info(resumed_ws_idx, resumed_internal_pane_id)
                .unwrap()
                .tokens
                .get("closing_completion")
                .map(String::as_str),
            Some("complete")
        );
    }

    #[test]
    fn report_agent_session_uses_addressed_shell_over_persisted_session_owner() {
        let (mut app, old_pane_id) = app_with_test_workspace();
        let (old_ws_idx, old_internal_pane_id) = app.parse_pane_id(&old_pane_id).unwrap();
        let old_terminal_id = app.state.workspaces[old_ws_idx]
            .pane_state(old_internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        bind_test_agent_session(
            &mut app,
            &old_pane_id,
            "herdr:claude",
            "claude",
            "persisted-session",
        );
        let old_state = app.state.terminals[&old_terminal_id].raw_agent_state();

        let addressed_pane_id = add_test_workspace(&mut app, "addressed-shell");
        let (addressed_ws_idx, addressed_internal_pane_id) =
            app.parse_pane_id(&addressed_pane_id).unwrap();
        let addressed_terminal_id = app.state.workspaces[addressed_ws_idx]
            .pane_state(addressed_internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();

        let response = app.handle_pane_report_agent_session(
            "shell-session-report".into(),
            PaneReportAgentSessionParams {
                pane_id: addressed_pane_id,
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("persisted-session".into()),
                agent_session_path: None,
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&addressed_terminal_id].current_agent_session_id(),
            Some("persisted-session")
        );
        assert_eq!(
            app.state.terminals[&old_terminal_id].raw_agent_state(),
            old_state
        );
        assert!(app.state.terminals[&old_terminal_id]
            .hook_authority
            .is_none());
    }

    #[test]
    fn report_agent_does_not_reroute_live_conflict_to_persisted_session_owner() {
        let (mut app, persisted_pane_id) = app_with_test_workspace();
        let (persisted_ws_idx, persisted_internal_pane_id) =
            app.parse_pane_id(&persisted_pane_id).unwrap();
        let persisted_terminal_id = app.state.workspaces[persisted_ws_idx]
            .pane_state(persisted_internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        bind_test_agent_session(
            &mut app,
            &persisted_pane_id,
            "herdr:claude",
            "claude",
            "persisted-session",
        );
        let persisted_state = app.state.terminals[&persisted_terminal_id].raw_agent_state();

        let live_pane_id = add_test_workspace(&mut app, "live-other-session");
        let (live_ws_idx, live_internal_pane_id) = app.parse_pane_id(&live_pane_id).unwrap();
        let live_terminal_id = bind_test_live_agent_session(
            &mut app,
            &live_pane_id,
            "herdr:claude",
            "claude",
            "live-session",
        );
        let live_state = app.state.terminals[&live_terminal_id].raw_agent_state();

        let mut report = closing_block_report(&live_pane_id, 1, vec![test_gate()]);
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("persisted-session".into());
        let response = app.handle_pane_report_agent("persisted-conflict".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&persisted_terminal_id].raw_agent_state(),
            persisted_state
        );
        assert!(app.state.terminals[&persisted_terminal_id]
            .hook_authority
            .is_none());
        assert_eq!(
            app.state.terminals[&live_terminal_id].current_agent_session_id(),
            Some("live-session")
        );
        assert_eq!(
            app.state.terminals[&live_terminal_id].raw_agent_state(),
            live_state
        );
        assert!(app
            .pane_info(live_ws_idx, live_internal_pane_id)
            .unwrap()
            .gates
            .is_empty());
    }

    #[test]
    fn report_agent_on_reused_pane_id_cannot_take_another_sessions_report() {
        let (mut app, reused_pane_id) = app_with_test_workspace();
        let reused_terminal_id = bind_test_live_agent_session(
            &mut app,
            &reused_pane_id,
            "herdr:claude",
            "claude",
            "other-session",
        );

        let owner_pane_id = add_test_workspace(&mut app, "actual-session-owner");
        let owner_terminal_id = bind_test_live_agent_session(
            &mut app,
            &owner_pane_id,
            "herdr:claude",
            "claude",
            "wanted-session",
        );
        let owner_internal_id = app.parse_pane_id(&owner_pane_id).unwrap().1;

        let mut report = closing_block_report(&reused_pane_id, 1, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("wanted-session".into());
        let response = app.handle_pane_report_agent("reused-pane-id-report".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&reused_terminal_id].current_agent_session_id(),
            Some("other-session")
        );
        assert_eq!(
            app.state.terminals[&reused_terminal_id].raw_agent_state(),
            AgentState::Working
        );
        let owner = app.pane_info(1, owner_internal_id).unwrap();
        // The owner pane is not active, so a completed report reads Done, same as a direct report.
        assert_eq!(owner.agent_status, crate::api::schema::AgentStatus::Done);
        assert_eq!(
            owner.tokens.get("closing_completion").map(String::as_str),
            Some("complete")
        );
        assert_eq!(
            app.state.terminals[&owner_terminal_id].raw_agent_state(),
            AgentState::Idle
        );
    }

    #[test]
    fn report_agent_session_replaces_same_agent_ref_less_live_authority_on_addressed_pane() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id =
            bind_test_live_agent_authority(&mut app, &pane_id, "herdr:claude", "claude", None);
        assert!(app.state.terminals[&terminal_id]
            .effective_non_retired_hook_authority()
            .is_some_and(|authority| authority.session_ref.is_none()));

        let response = app.handle_pane_report_agent_session(
            "replacement".into(),
            PaneReportAgentSessionParams {
                pane_id,
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(2),
                agent_session_id: Some("new-session".into()),
                agent_session_path: None,
                session_start_source: Some("clear".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id].current_agent_session_id(),
            Some("new-session")
        );
    }

    #[test]
    fn report_agent_prefers_live_owner_over_addressed_persisted_identity() {
        let (mut app, addressed_pane_id) = app_with_test_workspace();
        let addressed_terminal_id = bind_test_agent_session(
            &mut app,
            &addressed_pane_id,
            "herdr:claude",
            "claude",
            "shared-session",
        );
        let addressed_state = app.state.terminals[&addressed_terminal_id].raw_agent_state();

        let owner_pane_id = add_test_workspace(&mut app, "live-session-owner");
        let owner_terminal_id = bind_test_live_agent_session(
            &mut app,
            &owner_pane_id,
            "herdr:claude",
            "claude",
            "shared-session",
        );
        let (owner_ws_idx, owner_internal_id) = app.parse_pane_id(&owner_pane_id).unwrap();

        let mut report = closing_block_report(&addressed_pane_id, 2, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("shared-session".into());
        let response = app.handle_pane_report_agent("persisted-address".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.state.terminals[&addressed_terminal_id].raw_agent_state(),
            addressed_state
        );
        assert!(app.state.terminals[&addressed_terminal_id]
            .hook_authority
            .is_none());
        assert_eq!(
            app.state.terminals[&addressed_terminal_id]
                .persisted_agent_session
                .as_ref()
                .map(|session| session.session_ref.value.as_str()),
            Some("shared-session")
        );
        assert_eq!(
            app.pane_info(owner_ws_idx, owner_internal_id)
                .unwrap()
                .tokens
                .get("closing_completion")
                .map(String::as_str),
            Some("complete")
        );
        assert_eq!(
            app.state.terminals[&owner_terminal_id].raw_agent_state(),
            AgentState::Idle
        );
    }

    #[test]
    fn missing_report_pane_prefers_live_owner_over_persisted_duplicate() {
        let (mut app, persisted_pane_id) = app_with_test_workspace();
        bind_test_agent_session(
            &mut app,
            &persisted_pane_id,
            "herdr:claude",
            "claude",
            "shared-session",
        );
        let live_pane_id = add_test_workspace(&mut app, "live-session-owner");
        bind_test_live_agent_session(
            &mut app,
            &live_pane_id,
            "herdr:claude",
            "claude",
            "shared-session",
        );
        let live_target = app.parse_pane_id(&live_pane_id).unwrap();

        assert!(matches!(
            app.resolve_agent_report_pane(None, "claude", Some("shared-session")),
            AgentReportPaneResolution::Pane(target) if target == live_target
        ));
    }

    #[test]
    fn report_agent_on_reused_pane_id_conflicts_with_live_path_session() {
        let (mut app, reused_pane_id) = app_with_test_workspace();
        let path_ref = crate::agent_resume::AgentSessionRef::path("/tmp/pi-session.json")
            .expect("valid test session path");
        let pi_terminal_id = bind_test_live_agent_authority(
            &mut app,
            &reused_pane_id,
            "herdr:pi",
            "pi",
            Some(path_ref.clone()),
        );
        let pi_authority = app.state.terminals[&pi_terminal_id].hook_authority.clone();
        assert_eq!(
            pi_authority
                .as_ref()
                .and_then(|authority| authority.session_ref.as_ref()),
            Some(&path_ref)
        );

        let owner_pane_id = add_test_workspace(&mut app, "path-session-report-owner");
        let owner_terminal_id = bind_test_live_agent_session(
            &mut app,
            &owner_pane_id,
            "herdr:claude",
            "claude",
            "wanted-path-session-report",
        );
        let owner_internal_id = app.parse_pane_id(&owner_pane_id).unwrap().1;

        let mut report = closing_block_report(&reused_pane_id, 1, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("wanted-path-session-report".into());
        let response = app.handle_pane_report_agent("path-session-conflict".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let pi_terminal = &app.state.terminals[&pi_terminal_id];
        assert_eq!(pi_terminal.raw_agent_state(), AgentState::Working);
        assert_eq!(pi_terminal.hook_authority, pi_authority);
        assert_eq!(
            pi_terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| &session.session_ref),
            Some(&path_ref)
        );
        let owner = app.pane_info(1, owner_internal_id).unwrap();
        // The owner pane is not active, so a completed report reads Done, same as a direct report.
        assert_eq!(owner.agent_status, crate::api::schema::AgentStatus::Done);
        assert_eq!(
            owner.tokens.get("closing_completion").map(String::as_str),
            Some("complete")
        );
        assert_eq!(
            app.state.terminals[&owner_terminal_id].raw_agent_state(),
            AgentState::Idle
        );
    }

    #[test]
    fn report_agent_on_reused_pane_id_conflicts_with_live_ref_less_authority() {
        let (mut app, reused_pane_id) = app_with_test_workspace();
        let pi_terminal_id =
            bind_test_live_agent_authority(&mut app, &reused_pane_id, "herdr:pi", "pi", None);
        let pi_authority = app.state.terminals[&pi_terminal_id].hook_authority.clone();
        assert!(pi_authority
            .as_ref()
            .is_some_and(|authority| authority.session_ref.is_none()));

        let owner_pane_id = add_test_workspace(&mut app, "ref-less-report-owner");
        let owner_terminal_id = bind_test_live_agent_session(
            &mut app,
            &owner_pane_id,
            "herdr:claude",
            "claude",
            "wanted-ref-less-report",
        );
        let owner_internal_id = app.parse_pane_id(&owner_pane_id).unwrap().1;

        let mut report = closing_block_report(&reused_pane_id, 1, Vec::new());
        report.source = "herdr:claude-closing-block".into();
        report.agent = "claude".into();
        report.agent_session_id = Some("wanted-ref-less-report".into());
        let response = app.handle_pane_report_agent("ref-less-conflict".into(), report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let pi_terminal = &app.state.terminals[&pi_terminal_id];
        assert_eq!(pi_terminal.raw_agent_state(), AgentState::Working);
        assert_eq!(pi_terminal.hook_authority, pi_authority);
        assert!(pi_terminal
            .hook_authority
            .as_ref()
            .is_some_and(|authority| authority.session_ref.is_none()));
        let owner = app.pane_info(1, owner_internal_id).unwrap();
        // The owner pane is not active, so a completed report reads Done, same as a direct report.
        assert_eq!(owner.agent_status, crate::api::schema::AgentStatus::Done);
        assert_eq!(
            owner.tokens.get("closing_completion").map(String::as_str),
            Some("complete")
        );
        assert_eq!(
            app.state.terminals[&owner_terminal_id].raw_agent_state(),
            AgentState::Idle
        );
    }

    #[test]
    fn unknown_or_ambiguous_session_owner_keeps_pane_not_found() {
        let (mut app, _) = app_with_test_workspace();
        let unknown_report = PaneReportAgentParams {
            pane_id: "missing-workspace:p15".into(),
            source: "herdr:claude".into(),
            agent: "claude".into(),
            state: crate::api::schema::PaneAgentState::Idle,
            v: None,
            message: None,
            seq: None,
            wait: None,
            eta_s: None,
            reported_at: None,
            agent_session_id: Some("unknown-session".into()),
            agent_session_path: None,
            gates: None,
            items: None,
            decisions: None,
            completion: None,
            external_wait: None,
            parse_status: None,
            workers_unknown: None,
            agents: None,
            last_turn_at: None,
            settle_ready: None,
        };

        assert_eq!(
            metadata_error_code(
                &app.handle_pane_report_agent("unknown-pane-report".into(), unknown_report,)
            ),
            "pane_not_found"
        );

        let first_owner = add_test_workspace(&mut app, "first-owner");
        bind_test_agent_session(
            &mut app,
            &first_owner,
            "herdr:claude",
            "claude",
            "duplicate-session",
        );
        let second_owner = add_test_workspace(&mut app, "second-owner");
        bind_test_agent_session(
            &mut app,
            &second_owner,
            "herdr:claude",
            "claude",
            "duplicate-session",
        );
        let mut ambiguous_report = closing_block_report("missing-workspace:p15", 1, Vec::new());
        ambiguous_report.source = "herdr:claude-closing-block".into();
        ambiguous_report.agent = "claude".into();
        ambiguous_report.agent_session_id = Some("duplicate-session".into());
        assert_eq!(
            metadata_error_code(
                &app.handle_pane_report_agent("ambiguous-pane-report".into(), ambiguous_report,)
            ),
            "pane_not_found"
        );

        let session_response = app.handle_pane_report_agent_session(
            "unknown-session-report".into(),
            PaneReportAgentSessionParams {
                pane_id: "missing-workspace:p15".into(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: None,
                agent_session_id: Some("unknown-session".into()),
                agent_session_path: None,
                session_start_source: None,
            },
        );
        assert_eq!(metadata_error_code(&session_response), "pane_not_found");
    }

    #[test]
    fn clear_retires_the_previous_sessions_closing_report() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        let response = app.handle_pane_report_agent_session(
            "old-session".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(1),
                agent_session_id: Some("session-old".into()),
                agent_session_path: None,
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let mut old_report = closing_block_report(&pane_id, 2, vec![test_gate()]);
        old_report.agent_session_id = Some("session-old".into());
        old_report.completion = Some(crate::api::schema::ClosingCompletion::Complete);
        let response = app.handle_pane_report_agent("old-report".into(), old_report);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(app.pane_info(0, internal_pane_id).unwrap().gates.len(), 1);

        let response = app.handle_pane_report_agent_session(
            "new-session".into(),
            PaneReportAgentSessionParams {
                pane_id,
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(3),
                agent_session_id: Some("session-new".into()),
                agent_session_path: None,
                session_start_source: Some("clear".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let pane = app.pane_info(0, internal_pane_id).unwrap();
        assert!(pane.gates.is_empty());
        assert!(pane.items.is_empty());
        assert_eq!(
            pane.agent_status,
            crate::api::schema::AgentStatus::Working,
            "the old report cannot leave the new session Blocked or Done"
        );
        assert!(!pane.tokens.contains_key("closing_completion"));
    }

    #[test]
    fn delayed_installed_v2_reports_cannot_restore_a_replaced_session() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state.active = None;
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Working);

        let session = |session_id: &str, seq: u64| PaneReportAgentSessionParams {
            pane_id: pane_id.clone(),
            source: "herdr:codex-closing-block".into(),
            agent: "codex".into(),
            seq: Some(seq),
            agent_session_id: Some(session_id.into()),
            agent_session_path: None,
            session_start_source: None,
        };
        let report = |seq: u64,
                      gates: Vec<crate::api::schema::ClosingBlockItem>,
                      completion: crate::api::schema::ClosingCompletion| {
            PaneReportAgentParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex-closing-block".into(),
                agent: "codex".into(),
                state: crate::api::schema::PaneAgentState::Idle,
                v: Some(2),
                message: None,
                seq: Some(seq),
                wait: None,
                eta_s: None,
                reported_at: None,
                agent_session_id: None,
                agent_session_path: None,
                gates: Some(gates),
                items: Some(Vec::new()),
                decisions: Some(Vec::new()),
                completion: Some(completion),
                external_wait: None,
                parse_status: Some(crate::api::schema::ClosingParseStatus::Ok),
                workers_unknown: Some(false),
                agents: Some(0),
                last_turn_at: None,
                settle_ready: None,
            }
        };
        let metadata = |seq: u64| PaneReportMetadataParams {
            pane_id: pane_id.clone(),
            source: "herdr:codex-closing-block".into(),
            agent: None,
            applies_to_source: Some("herdr:codex-closing-block".into()),
            agent_session_id: None,
            title: None,
            work_context: None,
            work_title_long_brief: false,
            display_agent: None,
            state_labels: std::collections::HashMap::new(),
            tokens: std::collections::HashMap::from([
                ("closing_idle".into(), Some("1".into())),
                ("closing_contract".into(), Some("finish the task".into())),
                ("closing_contract_met".into(), Some("1".into())),
            ]),
            clear_title: false,
            clear_display_agent: false,
            clear_state_labels: false,
            seq: Some(seq),
            ttl_ms: None,
        };

        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent_session(
            "native-old".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(0),
                agent_session_id: Some("old".into()),
                agent_session_path: None,
                session_start_source: Some("startup".into()),
            },
        ))
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent_session("old-precursor".into(), session("old", 1)),
        )
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent(
            "old-report".into(),
            report(
                2,
                vec![test_gate()],
                crate::api::schema::ClosingCompletion::Incomplete,
            ),
        ))
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("old-metadata".into(), metadata(2)),
        )
        .unwrap();
        assert_eq!(app.pane_info(0, internal_pane_id).unwrap().gates.len(), 1);
        assert_eq!(
            app.state.terminals[&terminal_id].closing_contract_met(),
            Some(true)
        );

        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent_session(
            "replacement".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(3),
                agent_session_id: Some("new".into()),
                agent_session_path: None,
                session_start_source: Some("clear".into()),
            },
        ))
        .unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id].current_agent_session_id(),
            Some("new")
        );
        assert!(app.state.terminals[&terminal_id].legacy_closing_report_requires_guard());
        assert!(app.pane_info(0, internal_pane_id).unwrap().gates.is_empty());
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(Some(1));

        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent_session("late-gate-precursor".into(), session("old", 9)),
        )
        .unwrap();
        assert!(app.state.terminals[&terminal_id]
            .correlate_legacy_closing_report("herdr:codex-closing-block", "codex", Some(10),)
            .is_err());
        let mut late_gate = report(
            10,
            vec![test_gate()],
            crate::api::schema::ClosingCompletion::Incomplete,
        );
        late_gate.state = crate::api::schema::PaneAgentState::Blocked;
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent("late-gate".into(), late_gate))
                .unwrap();
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("late-contract".into(), metadata(10)),
        )
        .unwrap();
        assert!(app.pane_info(0, internal_pane_id).unwrap().gates.is_empty());
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Working,
            "a rejected stale report cannot restore Blocked"
        );
        assert_eq!(
            app.state.terminals[&terminal_id].closing_contract_met(),
            None
        );

        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent_session(
            "late-complete-precursor".into(),
            session("old", 11),
        ))
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent(
            "late-complete".into(),
            report(
                12,
                Vec::new(),
                crate::api::schema::ClosingCompletion::Complete,
            ),
        ))
        .unwrap();
        let seen = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .seen;
        let now = std::time::Instant::now();
        let _ = app
            .state
            .update_terminal_state_at(internal_pane_id, now, |terminal| {
                terminal
                    .set_active_subagents_with_projection_at(Some(0), seen, now)
                    .1
            });

        let pane = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap();
        let terminal = &app.state.terminals[&terminal_id];
        assert_eq!(
            app.pane_info(0, internal_pane_id).unwrap().agent_status,
            crate::api::schema::AgentStatus::Working,
            "a rejected stale completion cannot restore Idle"
        );
        assert_ne!(pane.agent_projection(terminal).status_key(), "done");
        assert!(pane.seen);
        assert!(pane.done_since.is_none());

        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_agent_session("current-precursor".into(), session("new", 13)),
        )
        .unwrap();
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_agent_session(
                "delayed-old-precursor".into(),
                session("old", 15),
            ))
            .unwrap();
        let _: SuccessResponse = serde_json::from_str(&app.handle_pane_report_agent(
            "current-report".into(),
            report(
                14,
                vec![test_gate()],
                crate::api::schema::ClosingCompletion::Incomplete,
            ),
        ))
        .unwrap();
        let _: SuccessResponse = serde_json::from_str(
            &app.handle_pane_report_metadata("current-metadata".into(), metadata(14)),
        )
        .unwrap();
        assert_eq!(app.pane_info(0, internal_pane_id).unwrap().gates.len(), 1);
        assert_eq!(
            app.state.terminals[&terminal_id].closing_contract_met(),
            Some(true)
        );
    }

    #[test]
    fn claude_session_report_retains_only_validated_runtime_transcript_path() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Idle,
            );
        let session_id = "6be5e8e1-cce2-4c1e-b04c-62e3e38eb75a";
        let transcript_dir = std::env::temp_dir().join("profiles/team-a/projects/-tmp-repro");
        let transcript = transcript_dir.join(format!("{session_id}.jsonl"));
        let response = app.handle_pane_report_agent_session(
            "claude-session".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(10),
                agent_session_id: Some(session_id.into()),
                agent_session_path: Some(transcript.display().to_string()),
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .claude_transcript_path
                .as_deref(),
            Some(transcript.as_path())
        );
        assert_eq!(
            app.state.terminals[&terminal_id]
                .claude_transcript_session_id
                .as_deref(),
            Some(session_id)
        );

        let pathless = app.handle_pane_report_agent_session(
            "pathless".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(11),
                agent_session_id: Some(session_id.into()),
                agent_session_path: None,
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&pathless).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .claude_transcript_path
                .as_deref(),
            Some(transcript.as_path()),
            "pathless reports for the same session must retain the exact hook path"
        );

        let spoofed = app.handle_pane_report_agent_session(
            "spoofed".into(),
            PaneReportAgentSessionParams {
                pane_id,
                source: "custom:claude".into(),
                agent: "claude".into(),
                seq: Some(12),
                agent_session_id: Some("other-session".into()),
                agent_session_path: Some(
                    transcript_dir
                        .join("other-session.jsonl")
                        .display()
                        .to_string(),
                ),
                session_start_source: Some("new".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&spoofed).unwrap();
        assert_eq!(
            app.state.terminals[&terminal_id]
                .claude_transcript_path
                .as_deref(),
            Some(transcript.as_path()),
            "an untrusted source cannot replace the accepted transcript target"
        );

        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_active_subagents(Some(2));
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state_with_visible_blocker(
                Some(crate::detect::Agent::Claude),
                crate::detect::AgentState::Unknown,
                false,
                false,
                true,
            );
        assert!(app.state.terminals[&terminal_id]
            .claude_transcript_path
            .is_none());
        assert!(app.state.terminals[&terminal_id]
            .claude_transcript_session_id
            .is_none());
        assert_eq!(app.state.terminals[&terminal_id].active_subagents, None);
    }

    #[test]
    fn pane_rename_writes_claude_name_once_and_inbound_reread_does_not_loop() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let root = session_name_temp_dir("claude-pane-rename");
        let transcript_dir = root.join("projects/-tmp-repro");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let session_id = "claude-session";
        let transcript = transcript_dir.join(format!("{session_id}.jsonl"));
        std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();
        let session_response = app.handle_pane_report_agent_session(
            "bind".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some(session_id.into()),
                agent_session_path: Some(transcript.display().to_string()),
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&session_response).unwrap();

        let rename_response = app.handle_pane_rename(
            "rename".into(),
            PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("Review billing retries".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&rename_response).unwrap();
        let written = std::fs::read_to_string(&transcript).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(written.lines().last().unwrap()).unwrap();
        assert_eq!(record["type"], "custom-title");
        assert_eq!(record["customTitle"], "Review billing retries");
        assert_eq!(record["sessionId"], session_id);

        let inbound = crate::work_title::request_from_session_name(
            crate::work_title::WorkTitleProvider::Claude,
            Some(&pane_id),
            &format!(r#"{{"hook_event_name":"Stop","session_id":"{session_id}"}}"#),
            &written,
            2,
        )
        .unwrap();
        let inbound_response = app.handle_pane_report_metadata("inbound".into(), inbound);
        let _: SuccessResponse = serde_json::from_str(&inbound_response).unwrap();
        assert_eq!(std::fs::read_to_string(&transcript).unwrap(), written);
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .session_name
                .as_deref(),
            Some("Review billing retries")
        );
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:claude".into(),
                agent: "claude".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("replacement-session")
                    .unwrap(),
            });
        assert!(app.state.terminals[&terminal_id]
            .session_name_write_target()
            .is_none());
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn profile_codex_path_rebinds_a_pathless_session_and_drives_rename() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let root = session_name_temp_dir("codex-profile-pane-rename");
        let pane_codex_home = root.join("pane-profile");
        let server_codex_home = root.join("server-profile");
        std::fs::create_dir_all(&pane_codex_home).unwrap();
        std::fs::create_dir_all(&server_codex_home).unwrap();
        let index = pane_codex_home.join("session_index.jsonl");
        let server_index = server_codex_home.join("session_index.jsonl");
        let session_id = "codex-thread";
        std::fs::write(
            &index,
            format!(r#"{{"id":"{session_id}","thread_name":"Original thread"}}"#) + "\n",
        )
        .unwrap();
        let server_index_contents =
            format!(r#"{{"id":"{session_id}","thread_name":"Wrong profile"}}"#) + "\n";
        std::fs::write(&server_index, &server_index_contents).unwrap();
        let pathless_response = app.handle_pane_report_agent_session(
            "pathless-bind".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(1),
                agent_session_id: Some(session_id.into()),
                agent_session_path: None,
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&pathless_response).unwrap();
        assert!(app.state.terminals[&terminal_id]
            .session_name_write_target()
            .is_none());

        let profile_path_response = app.handle_pane_report_agent_session(
            "profile-path".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(2),
                agent_session_id: Some(session_id.into()),
                agent_session_path: Some(index.display().to_string()),
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&profile_path_response).unwrap();

        let rename_response = app.handle_pane_rename(
            "rename".into(),
            PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("Audit session naming".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&rename_response).unwrap();
        let index_contents = std::fs::read_to_string(&index).unwrap();
        let record: serde_json::Value =
            serde_json::from_str(index_contents.lines().last().unwrap()).unwrap();
        assert_eq!(record["id"], session_id);
        assert_eq!(record["thread_name"], "Audit session naming");
        assert!(record["updated_at"].as_str().unwrap().ends_with('Z'));
        assert_eq!(
            std::fs::read_to_string(&server_index).unwrap(),
            server_index_contents,
            "the server must not resolve a Codex index from its own profile"
        );

        let invalid_contents = "{\"id\":\"other-thread\",\"thread_name\":\"Other name\"}\n";
        std::fs::write(&index, invalid_contents).unwrap();
        let repeated_response = app.handle_pane_report_agent_session(
            "repeat-bind".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(3),
                agent_session_id: Some(session_id.into()),
                agent_session_path: None,
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&repeated_response).unwrap();
        assert!(app.state.terminals[&terminal_id]
            .session_name_write_target()
            .is_some());
        let failed_write_response = app.handle_pane_rename(
            "failed-write".into(),
            PaneRenameParams {
                pane_id,
                label: Some("Local name after invalid index".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&failed_write_response).unwrap();
        assert_eq!(std::fs::read_to_string(&index).unwrap(), invalid_contents);
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Local name after invalid index")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn invalid_reported_path_clears_the_previous_write_target() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Codex), AgentState::Idle);

        let root = session_name_temp_dir("codex-target-rebind");
        let valid_index = root.join("session_index.jsonl");
        let invalid_dir = root.join("replacement");
        std::fs::create_dir_all(&invalid_dir).unwrap();
        let invalid_index = invalid_dir.join("session_index.jsonl");
        std::fs::write(
            &valid_index,
            b"{\"id\":\"codex-thread\",\"thread_name\":\"Original thread\"}\n",
        )
        .unwrap();
        std::fs::write(&invalid_index, b"{\"id\":\"other-thread\"}\n").unwrap();

        for (seq, path) in [(1, Some(valid_index.display().to_string())), (2, None)] {
            let response = app.handle_pane_report_agent_session(
                format!("bind-{seq}"),
                PaneReportAgentSessionParams {
                    pane_id: pane_id.clone(),
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    seq: Some(seq),
                    agent_session_id: Some("codex-thread".into()),
                    agent_session_path: path,
                    session_start_source: None,
                },
            );
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        }
        assert!(app.state.terminals[&terminal_id]
            .session_name_write_target()
            .is_some());

        let invalid_response = app.handle_pane_report_agent_session(
            "invalid-rebind".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:codex".into(),
                agent: "codex".into(),
                seq: Some(3),
                agent_session_id: Some("codex-thread".into()),
                agent_session_path: Some(invalid_index.display().to_string()),
                session_start_source: None,
            },
        );
        let _: SuccessResponse = serde_json::from_str(&invalid_response).unwrap();
        assert!(app.state.terminals[&terminal_id]
            .session_name_write_target()
            .is_none());

        let before = std::fs::read_to_string(&valid_index).unwrap();
        let rename_response = app.handle_pane_rename(
            "rename".into(),
            PaneRenameParams {
                pane_id,
                label: Some("Do not write stale target".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&rename_response).unwrap();
        assert_eq!(std::fs::read_to_string(&valid_index).unwrap(), before);
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn pane_rename_survives_write_failure_and_single_token_names_do_not_write() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (_, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[0]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_detected_state(Some(Agent::Claude), AgentState::Idle);

        let root = session_name_temp_dir("rename-failure");
        let transcript_dir = root.join("projects/-tmp-repro");
        std::fs::create_dir_all(&transcript_dir).unwrap();
        let transcript = transcript_dir.join("claude-session.jsonl");
        std::fs::write(&transcript, b"{\"type\":\"user\"}\n").unwrap();
        let session_response = app.handle_pane_report_agent_session(
            "bind".into(),
            PaneReportAgentSessionParams {
                pane_id: pane_id.clone(),
                source: "herdr:claude".into(),
                agent: "claude".into(),
                seq: Some(1),
                agent_session_id: Some("claude-session".into()),
                agent_session_path: Some(transcript.display().to_string()),
                session_start_source: Some("startup".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&session_response).unwrap();

        let before = std::fs::read_to_string(&transcript).unwrap();
        let token_response = app.handle_pane_rename(
            "token".into(),
            PaneRenameParams {
                pane_id: pane_id.clone(),
                label: Some("random-id".into()),
            },
        );
        let _: SuccessResponse = serde_json::from_str(&token_response).unwrap();
        assert_eq!(std::fs::read_to_string(&transcript).unwrap(), before);

        std::fs::remove_file(&transcript).unwrap();
        let failure_response = app.handle_pane_rename(
            "failure".into(),
            PaneRenameParams {
                pane_id,
                label: Some("Local rename survives".into()),
            },
        );
        let response: SuccessResponse = serde_json::from_str(&failure_response).unwrap();
        assert!(matches!(response.result, ResponseResult::PaneInfo { .. }));
        assert_eq!(
            app.state.terminals[&terminal_id].manual_label.as_deref(),
            Some("Local rename survives")
        );
        std::fs::remove_dir_all(root).unwrap();
    }

    #[tokio::test]
    async fn initial_agent_session_assignment_does_not_clear_retained_title() {
        let (mut app, pane_id) = app_with_test_workspace();
        let (workspace_idx, internal_pane_id) = app.parse_pane_id(&pane_id).unwrap();
        let terminal_id = app.state.workspaces[workspace_idx]
            .pane_state(internal_pane_id)
            .unwrap()
            .attached_terminal_id
            .clone();
        app.state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .detected_agent = Some(crate::detect::Agent::Codex);
        let runtime = crate::terminal::TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        app.terminal_runtimes.insert(terminal_id.clone(), runtime);

        app.terminal_runtimes
            .get(&terminal_id)
            .unwrap()
            .test_process_pty_bytes(b"\x1b]0;Initial session title\x07");
        let response = app.handle_api_request(crate::api::schema::Request {
            id: "session-a".into(),
            method: crate::api::schema::Method::PaneReportAgentSession(
                PaneReportAgentSessionParams {
                    pane_id,
                    source: "herdr:codex".into(),
                    agent: "codex".into(),
                    seq: Some(1),
                    agent_session_id: Some("session-a".into()),
                    agent_session_path: None,
                    session_start_source: Some("startup".into()),
                },
            ),
        });
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert_eq!(
            app.terminal_runtimes
                .get(&terminal_id)
                .unwrap()
                .terminal_title()
                .as_deref(),
            Some("Initial session title")
        );
    }

    #[test]
    fn ac5_unguarded_metadata_cannot_replace_hook_work_context() {
        let (mut app, pane_id) = app_with_test_workspace();
        let terminal_id = app.state.workspaces[0]
            .pane_state(app.state.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .attached_terminal_id
            .clone();
        let mut params = metadata_params(pane_id);
        params.work_context = Some(crate::work_context::PaneWorkContext {
            ticket_ids: vec!["MAT-7".into()],
            work_title: params.title.clone(),
            ..Default::default()
        });

        let response = app.handle_pane_report_metadata("unguarded".into(), params);

        assert_eq!(metadata_error_code(&response), "invalid_work_context");
        assert_eq!(
            app.state.terminals[&terminal_id].effective_work_context(),
            &crate::work_context::PaneWorkContext::default()
        );
    }

    #[test]
    fn turn_start_fixture_reaches_guarded_herdr_title() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:codex",
            "codex",
            "fixture-codex-session",
        );
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_hook_authority_with_session_ref(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::id("fixture-codex-session"),
            Some(1),
        );
        let params = crate::work_title::request_from_turn_start(
            crate::work_title::WorkTitleProvider::Codex,
            Some(&pane_id),
            include_str!("../../../tests/fixtures/work-titles/codex-user-prompt-submit.json"),
            25,
        )
        .unwrap();

        let response = app.handle_pane_report_metadata("turn-start".into(), params);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        let metadata =
            &app.state.terminals[&terminal_id].agent_metadata[crate::work_title::WORK_TITLE_SOURCE];
        assert_eq!(
            metadata.title.as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert_eq!(metadata.agent_label.as_deref(), Some("codex"));
        assert_eq!(metadata.applies_to_source.as_deref(), Some("herdr:codex"));
        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert_eq!(
            app.agent_info(0, internal_pane_id)
                .and_then(|agent| agent.title)
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
    }

    #[test]
    fn terse_later_user_prompt_submit_retains_the_session_initial_title() {
        let (mut app, pane_id) = app_with_test_workspace();
        let internal_pane_id = app.state.workspaces[0].tabs[0].root_pane;
        let terminal_id = bind_test_agent_session(
            &mut app,
            &pane_id,
            "herdr:codex",
            "codex",
            "fixture-codex-session",
        );
        let terminal = app.state.terminals.get_mut(&terminal_id).unwrap();
        terminal.detected_agent = Some(Agent::Codex);
        terminal.set_hook_authority_with_session_ref(
            "herdr:codex".into(),
            "codex".into(),
            AgentState::Idle,
            None,
            crate::agent_resume::AgentSessionRef::id("fixture-codex-session"),
            Some(1),
        );

        let first = crate::work_title::request_from_turn_start(
            crate::work_title::WorkTitleProvider::Codex,
            Some(&pane_id),
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"fixture-codex-session","prompt":"Fix billing retry regression"}"#,
            25,
        )
        .unwrap();
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("first".into(), first)).unwrap();

        let later = crate::work_title::request_from_turn_start(
            crate::work_title::WorkTitleProvider::Codex,
            Some(&pane_id),
            r#"{"hook_event_name":"UserPromptSubmit","session_id":"fixture-codex-session","prompt":"do it"}"#,
            26,
        )
        .unwrap();
        let _: SuccessResponse =
            serde_json::from_str(&app.handle_pane_report_metadata("later".into(), later)).unwrap();

        assert_eq!(
            app.state.terminals[&terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert_eq!(
            app.state.terminals[&terminal_id].agent_metadata[crate::work_title::WORK_TITLE_SOURCE]
                .title
                .as_deref(),
            Some("Fix Billing Retry Regression")
        );
        assert!(app.state.workspaces[0].tabs[0].custom_name.is_none());
        assert!(app.agent_info(0, internal_pane_id).is_some());
    }

    #[test]
    fn guarded_work_titles_are_bound_to_each_agent_and_resume_session() {
        let (mut app, codex_pane) = app_with_test_workspace();
        app.state.workspaces.push(Workspace::test_new("claude"));
        app.state.ensure_test_terminals();
        let claude_internal_pane = app.state.workspaces[1].tabs[0].root_pane;
        let claude_pane = app.public_pane_id(1, claude_internal_pane).unwrap();
        let codex_terminal = bind_test_agent_session(
            &mut app,
            &codex_pane,
            "herdr:codex",
            "codex",
            "codex-session",
        );
        let claude_terminal = bind_test_agent_session(
            &mut app,
            &claude_pane,
            "herdr:claude",
            "claude",
            "claude-session",
        );

        for params in [
            guarded_work_title_params(
                codex_pane.clone(),
                "codex",
                "herdr:codex",
                "codex-session",
                "Audit Local CI Evidence",
                30,
            ),
            guarded_work_title_params(
                claude_pane,
                "claude",
                "herdr:claude",
                "claude-session",
                "Review Auth Migration Safety",
                30,
            ),
        ] {
            let response = app.handle_pane_report_metadata("agent".into(), params);
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        }
        assert_eq!(
            app.state.terminals[&codex_terminal].agent_metadata
                [crate::work_title::WORK_TITLE_SOURCE]
                .title
                .as_deref(),
            Some("Audit Local CI Evidence")
        );
        assert_eq!(
            app.state.terminals[&claude_terminal].agent_metadata
                [crate::work_title::WORK_TITLE_SOURCE]
                .title
                .as_deref(),
            Some("Review Auth Migration Safety")
        );

        bind_test_agent_session(
            &mut app,
            &codex_pane,
            "herdr:codex",
            "codex",
            "codex-resumed",
        );
        let resumed = guarded_work_title_params(
            codex_pane,
            "codex",
            "herdr:codex",
            "codex-resumed",
            "Measure Fleet Token Savings",
            31,
        );
        let response = app.handle_pane_report_metadata("resume".into(), resumed);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        assert_eq!(
            app.state.terminals[&codex_terminal].agent_metadata
                [crate::work_title::WORK_TITLE_SOURCE]
                .title
                .as_deref(),
            Some("Measure Fleet Token Savings")
        );
    }

    #[test]
    fn work_title_initial_briefings_are_session_scoped_and_followups_keep_the_title() {
        let (mut app, codex_pane) = app_with_test_workspace();
        app.state.workspaces.push(Workspace::test_new("claude"));
        app.state.ensure_test_terminals();
        let claude_pane = app
            .public_pane_id(1, app.state.workspaces[1].tabs[0].root_pane)
            .unwrap();
        bind_test_agent_session(
            &mut app,
            &codex_pane,
            "herdr:codex",
            "codex",
            "codex-session",
        );
        bind_test_agent_session(
            &mut app,
            &claude_pane,
            "herdr:claude",
            "claude",
            "claude-session",
        );

        for (id, provider, pane, session, prompt, seq) in [
            (
                "codex-first",
                crate::work_title::WorkTitleProvider::Codex,
                codex_pane.as_str(),
                "codex-session",
                "Review billing retry regression\n\nplease start",
                40,
            ),
            (
                "claude-first",
                crate::work_title::WorkTitleProvider::Claude,
                claude_pane.as_str(),
                "claude-session",
                "Audit plugin marketplace safety",
                40,
            ),
            (
                "codex-terse",
                crate::work_title::WorkTitleProvider::Codex,
                codex_pane.as_str(),
                "codex-session",
                "please do it now",
                41,
            ),
            (
                "claude-terse",
                crate::work_title::WorkTitleProvider::Claude,
                claude_pane.as_str(),
                "claude-session",
                "go ahead",
                41,
            ),
            (
                "codex-later",
                crate::work_title::WorkTitleProvider::Codex,
                codex_pane.as_str(),
                "codex-session",
                "Can we fix why this tab keeps its old name?",
                42,
            ),
        ] {
            let payload = serde_json::json!({
                "hook_event_name": "UserPromptSubmit",
                "session_id": session,
                "prompt": prompt,
            })
            .to_string();
            let params =
                crate::work_title::request_from_turn_start(provider, Some(pane), &payload, seq)
                    .unwrap();
            let response = app.handle_pane_report_metadata(id.into(), params);
            let _: SuccessResponse = serde_json::from_str(&response).unwrap();
        }

        assert_eq!(
            app.state.terminals[&app.state.workspaces[0]
                .pane_state(app.state.workspaces[0].tabs[0].root_pane)
                .unwrap()
                .attached_terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Review Billing Retry Regression")
        );
        assert_eq!(
            app.state.terminals[&app.state.workspaces[1]
                .pane_state(app.state.workspaces[1].tabs[0].root_pane)
                .unwrap()
                .attached_terminal_id]
                .effective_work_context()
                .work_title
                .as_deref(),
            Some("Audit Plugin Marketplace Safety")
        );
        assert!(!app.state.terminals[&app.state.workspaces[0]
            .pane_state(app.state.workspaces[0].tabs[0].root_pane)
            .unwrap()
            .attached_terminal_id]
            .effective_work_context()
            .work_title
            .as_deref()
            .unwrap()
            .to_ascii_lowercase()
            .contains("can we fix why this tab keeps its old name"));
    }

    #[tokio::test]
    async fn guarded_work_title_does_not_touch_pane_input_or_process_state() {
        let (mut app, pane_id, mut input_rx) = app_with_send_key_runtime(8);
        let terminal_id =
            bind_test_agent_session(&mut app, &pane_id, "herdr:claude", "claude", "session-1");
        let process_state = (
            app.state.terminals[&terminal_id].detected_agent,
            app.state.terminals[&terminal_id].raw_agent_state(),
        );

        let params = guarded_work_title_params(
            pane_id,
            "claude",
            "herdr:claude",
            "session-1",
            "Review Auth Migration Safety",
            40,
        );
        let response = app.handle_pane_report_metadata("title".into(), params);
        let _: SuccessResponse = serde_json::from_str(&response).unwrap();

        assert!(input_rx.try_recv().is_err());
        assert_eq!(
            (
                app.state.terminals[&terminal_id].detected_agent,
                app.state.terminals[&terminal_id].raw_agent_state(),
            ),
            process_state
        );
    }
}
