use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::Arc;

use ratatui::layout::Direction;
use tokio::sync::{mpsc, Notify};
use tracing::{error, warn};

use crate::detect::AgentState;
use crate::events::AppEvent;
use crate::layout::{Node, PaneId, TileLayout};
use crate::pane::{PaneLaunchEnv, PaneState};
use crate::render_signal::RenderSignal;
use crate::terminal::{TerminalId, TerminalRuntime, TerminalState};
use crate::workspace::Workspace;

use super::snapshot::{
    PaneAgentSessionSnapshot, PaneHistorySnapshot, TabHistorySnapshot, WorkspaceHistorySnapshot,
};
use super::{
    DirectionSnapshot, LayoutSnapshot, SessionHistorySnapshot, SessionSnapshot, TabSnapshot,
    WorkspaceSnapshot,
};

struct AgentRestoreState<'a> {
    enabled: bool,
    resumed_sessions: &'a mut HashSet<String>,
}

struct PaneRestoreStartup<'a> {
    restore_plan: Option<crate::agent_resume::AgentResumePlan>,
    initial_history_ansi: Option<&'a str>,
    duplicate_agent_session: bool,
    reserved_agent_session: Option<String>,
}

struct RestoreRuntimeContext<'a> {
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'a>,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
}

type RestoredSession = (
    Vec<Workspace>,
    HashMap<TerminalId, TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredWorkspace = (
    Workspace,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
);
type RestoredTab = (
    crate::workspace::Tab,
    Vec<TerminalState>,
    HashMap<TerminalId, TerminalRuntime>,
    HashMap<PaneId, u32>,
);
type RestoreFailures<T> = (T, usize);

fn restore_pane_activity(pane: &mut PaneState, saved: Option<&super::snapshot::PaneSnapshot>) {
    let Some(saved) = saved else {
        return;
    };
    pane.is_companion = saved.is_companion;
    let now = std::time::Instant::now();
    let now_unix = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    if let Some(last_activity_at) = saved.last_activity_at {
        pane.activity
            .restore_unix_timestamp_at(last_activity_at, now, now_unix);
    }
    if let Some(detection_output_at) = saved.detection_output_at {
        pane.activity.restore_detection_output_unix_timestamp_at(
            detection_output_at,
            now,
            now_unix,
        );
    }
    if let Some(quiet_since_at) = saved.quiet_since_at {
        pane.activity
            .restore_quiet_unix_timestamp_at(quiet_since_at, now, now_unix);
    }
}

fn restore_auto_settle_state(
    terminal: &mut TerminalState,
    session: Option<&super::snapshot::PaneAgentSessionSnapshot>,
) {
    if let Some(session) = session {
        terminal.restore_auto_settle_state(
            &session.value,
            session.user_replied,
            session.auto_settle_turn_seq,
        );
    }
}

/// Restore workspaces from a snapshot. Each pane gets a fresh shell in its saved cwd.
pub fn restore(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    resume_agents_on_restore: bool,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    let mut imported_panes = HashMap::new();
    restore_with_imports(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        resume_agents_on_restore,
        &mut imported_panes,
        events,
        render_notify,
        render_dirty,
    )
}

#[cfg(unix)]
pub fn restore_handoff(
    snapshot: &SessionSnapshot,
    scrollback_limit_bytes: usize,
    default_shell: &str,
    shell_mode: crate::config::ShellModeConfig,
    imports: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    restore_with_imports_strict(
        snapshot,
        None,
        24,
        80,
        scrollback_limit_bytes,
        crate::pane::PaneShellConfig::new(default_shell, shell_mode),
        true,
        imports,
        events,
        render_notify,
        render_dirty,
    )
}

#[cfg(unix)]
pub fn handoff_pane_aliases(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
) -> HashMap<u32, PaneId> {
    let mut aliases = HashMap::new();
    for (ws_snap, workspace) in snapshot.workspaces.iter().zip(workspaces) {
        for (tab_snap, tab) in ws_snap.tabs.iter().zip(&workspace.tabs) {
            let old_ids = collect_snapshot_pane_ids(&tab_snap.layout);
            let new_ids = tab.layout.pane_ids();
            for (old_id, new_id) in old_ids.into_iter().zip(new_ids) {
                if old_id != new_id.raw() {
                    aliases.insert(old_id, new_id);
                }
            }
        }
    }
    aliases
}

#[cfg(unix)]
fn collect_snapshot_pane_ids(node: &LayoutSnapshot) -> Vec<u32> {
    let mut ids = Vec::new();
    collect_snapshot_ids_inner(node, &mut ids);
    ids
}

#[cfg(unix)]
fn collect_snapshot_ids_inner(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_snapshot_ids_inner(first, ids);
            collect_snapshot_ids_inner(second, ids);
        }
    }
}

fn migrated_public_pane_numbers_by_old_raw(
    snap: &WorkspaceSnapshot,
    next_public_pane_number: &mut usize,
) -> HashMap<u32, usize> {
    let mut public_numbers = snap.public_pane_numbers.clone();
    for tab in &snap.tabs {
        let mut pane_ids = Vec::new();
        collect_layout_snapshot_pane_ids(&tab.layout, &mut pane_ids);
        for old_raw in pane_ids {
            public_numbers.entry(old_raw).or_insert_with(|| {
                let number = *next_public_pane_number;
                *next_public_pane_number += 1;
                number
            });
        }
    }
    public_numbers
}

fn collect_layout_snapshot_pane_ids(node: &LayoutSnapshot, ids: &mut Vec<u32>) {
    match node {
        LayoutSnapshot::Pane(id) => ids.push(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            collect_layout_snapshot_pane_ids(first, ids);
            collect_layout_snapshot_pane_ids(second, ids);
        }
    }
}

#[cfg(unix)]
fn restore_with_imports_strict(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> std::io::Result<RestoredSession> {
    let (restored, failed_imports) = restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    );
    if failed_imports > 0 {
        return Err(std::io::Error::other(format!(
            "handoff failed to restore {failed_imports} imported pane runtime(s)"
        )));
    }
    if !imported_panes.is_empty() {
        return Err(std::io::Error::other(format!(
            "handoff import did not consume {} pane runtime(s)",
            imported_panes.len()
        )));
    }
    Ok(restored)
}

fn restore_with_imports(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoredSession {
    restore_with_imports_and_failures(
        snapshot,
        history,
        rows,
        cols,
        scrollback_limit_bytes,
        shell_config,
        resume_agents_on_restore,
        imported_panes,
        events,
        render_notify,
        render_dirty,
    )
    .0
}

fn restore_with_imports_and_failures(
    snapshot: &SessionSnapshot,
    history: Option<&SessionHistorySnapshot>,
    rows: u16,
    cols: u16,
    scrollback_limit_bytes: usize,
    shell_config: crate::pane::PaneShellConfig<'_>,
    resume_agents_on_restore: bool,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    events: mpsc::Sender<AppEvent>,
    render_notify: Arc<Notify>,
    render_dirty: Arc<RenderSignal>,
) -> RestoreFailures<RestoredSession> {
    let history = compatible_history(snapshot, history);
    let history = history.filter(|history| {
        let matches = history.layout_fingerprint.is_some()
            && history.layout_fingerprint == super::snapshot::layout_fingerprint(snapshot);
        if !matches {
            tracing::warn!("Ignoring pane history without a matching session layout");
        }
        matches
    });
    let mut workspaces = Vec::new();
    let mut terminals = HashMap::new();
    let mut terminal_runtimes = HashMap::new();
    let mut resumed_agent_sessions = HashSet::new();
    let mut failed_imports = 0;
    for (idx, ws_snap) in snapshot.workspaces.iter().enumerate() {
        let runtime_context = RestoreRuntimeContext {
            scrollback_limit_bytes,
            shell_config,
            resume_agents_on_restore,
            events: events.clone(),
            render_notify: render_notify.clone(),
            render_dirty: render_dirty.clone(),
        };
        let (restored, workspace_failed_imports) = restore_workspace(
            ws_snap,
            history.and_then(|history| history.workspaces.get(idx)),
            rows,
            cols,
            &runtime_context,
            &mut resumed_agent_sessions,
            imported_panes,
        );
        failed_imports += workspace_failed_imports;
        if let Some((workspace, restored_terminals, restored_runtimes)) = restored {
            for terminal in restored_terminals {
                terminals.insert(terminal.id.clone(), terminal);
            }
            terminal_runtimes.extend(restored_runtimes);
            workspaces.push(workspace);
        }
    }
    crate::workspace::reserve_workspace_ids(&workspaces);
    ((workspaces, terminals, terminal_runtimes), failed_imports)
}

fn restore_workspace(
    snap: &WorkspaceSnapshot,
    history: Option<&WorkspaceHistorySnapshot>,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
) -> RestoreFailures<Option<RestoredWorkspace>> {
    let mut tabs = Vec::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let workspace_id = snap
        .id
        .clone()
        .unwrap_or_else(crate::workspace::generate_workspace_id);
    let mut next_public_pane_number = snap
        .public_pane_numbers
        .values()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_pane_number);
    let public_pane_numbers_by_old_raw =
        migrated_public_pane_numbers_by_old_raw(snap, &mut next_public_pane_number);
    let public_pane_ids_by_old_raw: HashMap<u32, String> = public_pane_numbers_by_old_raw
        .iter()
        .map(|(old_raw, public_number)| {
            (
                *old_raw,
                format!(
                    "{}:p{}",
                    workspace_id,
                    crate::workspace::encode_public_number(*public_number)
                ),
            )
        })
        .collect();
    let mut public_pane_numbers = HashMap::new();
    let mut next_public_tab_number = snap
        .public_tab_numbers
        .iter()
        .copied()
        .max()
        .and_then(|max| max.checked_add(1))
        .unwrap_or(1)
        .max(snap.next_public_tab_number);
    let mut failed_imports = 0;

    for (idx, tab_snap) in snap.tabs.iter().enumerate() {
        let tab_number = snap.public_tab_numbers.get(idx).copied().unwrap_or(idx + 1);
        let (restored_tab, tab_failed_imports) = restore_tab(
            tab_snap,
            history.and_then(|history| history.tabs.get(idx)),
            tab_number,
            &workspace_id,
            rows,
            cols,
            runtime_context,
            resumed_agent_sessions,
            imported_panes,
            &public_pane_ids_by_old_raw,
        );
        failed_imports += tab_failed_imports;
        let Some((mut tab, restored_terminals, restored_runtimes, reverse_id_map)) = restored_tab
        else {
            continue;
        };
        if let Some(public_tab_number) = snap.public_tab_numbers.get(idx).copied() {
            tab.number = public_tab_number;
        }
        next_public_tab_number = next_public_tab_number.max(tab.number + 1);
        for pane_id in tab.layout.pane_ids() {
            let public_number = public_pane_numbers_by_old_raw
                .get(
                    &reverse_id_map
                        .get(&pane_id)
                        .copied()
                        .unwrap_or(pane_id.raw()),
                )
                .copied()
                .unwrap_or_else(|| {
                    let number = next_public_pane_number;
                    next_public_pane_number += 1;
                    number
                });
            public_pane_numbers.insert(pane_id, public_number);
            next_public_pane_number = next_public_pane_number.max(public_number + 1);
        }
        terminals.extend(restored_terminals);
        terminal_runtimes.extend(restored_runtimes);
        tabs.push(tab);
    }

    if tabs.is_empty() {
        return (None, failed_imports);
    }

    let worktree_space = restored_worktree_space_membership(snap.worktree_space.clone());
    let (cached_git_space, cached_auto_label, cached_git_status_key) =
        crate::workspace::discover_workspace_git_identity(&snap.identity_cwd);

    (
        Some(Workspace {
            id: workspace_id,
            is_fleet: snap.is_fleet,
            custom_name: snap.custom_name.clone(),
            identity_cwd: snap.identity_cwd.clone(),
            cached_identity_cwd: snap.identity_cwd.clone(),
            cached_auto_label,
            cached_git_status_key,
            cached_git_branch: crate::workspace::git_branch(&snap.identity_cwd),
            cached_git_ahead_behind: None,
            cached_git_space,
            worktree_space,
            repo_binding: snap.repo_binding.clone(),
            repo_binding_cleared: snap.repo_binding_cleared,
            metadata_tokens: crate::metadata_tokens::MetadataTokens::default(),
            metadata_token_sequences: HashMap::new(),
            public_pane_numbers,
            next_public_pane_number,
            next_public_tab_number,
            active_tab: snap.active_tab.min(tabs.len().saturating_sub(1)),
            tabs,
            #[cfg(test)]
            test_runtimes: HashMap::new(),
        })
        .map(|workspace| (workspace, terminals, terminal_runtimes)),
        failed_imports,
    )
}

fn unavailable_restored_terminal(
    pane: Option<&super::snapshot::PaneSnapshot>,
    cwd: PathBuf,
    reason: String,
) -> TerminalState {
    warn!(cwd = %cwd.display(), reason = %reason, "preserving unavailable restored pane");
    let mut terminal = TerminalState::new(TerminalId::alloc(), cwd);
    terminal.restore_error = Some(reason);
    if let Some(pane) = pane {
        terminal.manual_label = pane.label.clone();
        terminal.launch_argv = pane.launch_argv.clone();
        if let Some(session) = restored_terminal_agent_session(pane.agent_session.as_ref(), false) {
            terminal.set_persisted_agent_session(session);
            restore_auto_settle_state(&mut terminal, pane.agent_session.as_ref());
        }
        match (
            pane.agent_name.as_ref(),
            pane.managed_agent_kind
                .as_deref()
                .and_then(crate::detect::parse_canonical_agent_label),
        ) {
            (Some(name), Some(agent)) => terminal.restore_managed_agent(name.clone(), agent),
            (Some(name), None) => terminal.set_agent_name(name.clone()),
            _ => {}
        }
    }
    terminal
}

fn restored_worktree_space_membership(
    space: Option<crate::workspace::WorktreeSpaceMembership>,
) -> Option<crate::workspace::WorktreeSpaceMembership> {
    space.filter(|space| {
        space.checkout_path.exists()
            && crate::workspace::git_space_metadata(&space.checkout_path)
                .is_some_and(|current| current.key == space.key)
    })
}

fn restore_tab(
    snap: &TabSnapshot,
    history: Option<&TabHistorySnapshot>,
    number: usize,
    workspace_id: &str,
    rows: u16,
    cols: u16,
    runtime_context: &RestoreRuntimeContext<'_>,
    resumed_agent_sessions: &mut HashSet<String>,
    imported_panes: &mut HashMap<u32, crate::handoff_runtime::ImportedHandoffRuntime>,
    public_pane_ids_by_old_raw: &HashMap<u32, String>,
) -> RestoreFailures<Option<RestoredTab>> {
    let (node, id_map) = restore_node_remapped(&snap.layout);
    let reverse_id_map: HashMap<PaneId, u32> = id_map
        .iter()
        .map(|(&old_id, &new_id)| (new_id, old_id))
        .collect();
    let pane_ids = collect_pane_ids(&node);

    let mut panes = HashMap::new();
    let mut terminals = Vec::new();
    let mut terminal_runtimes = HashMap::new();
    let mut failed_imports = 0;
    for id in &pane_ids {
        let old_id = reverse_id_map.get(id);
        let saved_pane = old_id.and_then(|old_id| snap.panes.get(old_id));
        let saved_cwd = saved_pane
            .map(|p| p.cwd.clone())
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));

        let cwd = saved_cwd;
        let has_import = old_id.is_some_and(|old_id| imported_panes.contains_key(old_id));
        if !has_import && !cwd.is_dir() {
            let terminal = unavailable_restored_terminal(
                saved_pane,
                cwd,
                "Saved directory is unavailable. Restore the directory and restart this session."
                    .into(),
            );
            let mut pane = PaneState::new(terminal.id.clone());
            restore_pane_activity(&mut pane, saved_pane);
            panes.insert(*id, pane);
            terminals.push(terminal);
            continue;
        }

        let saved_label = saved_pane.and_then(|p| p.label.clone());
        let saved_work_context = saved_pane
            .map(|pane| pane.work_context.clone())
            .unwrap_or_default();
        let saved_work_context_tiers = saved_pane.and_then(|pane| pane.work_context_tiers.clone());
        let saved_agent_name = saved_pane.and_then(|p| p.agent_name.clone());
        let saved_managed_agent = saved_pane
            .and_then(|pane| pane.managed_agent_kind.as_deref())
            .and_then(crate::detect::parse_canonical_agent_label);
        let saved_launch_argv = saved_pane.and_then(|p| p.launch_argv.clone());
        let saved_agent_session = saved_pane.and_then(|p| p.agent_session.as_ref());
        let expected_pane_id = old_id.and_then(|old_id| public_pane_ids_by_old_raw.get(old_id));
        let saved_history = old_id
            .and_then(|old_id| history.and_then(|history| history.panes.get(old_id)))
            .and_then(|history| {
                history_for_pane(Some(history), expected_pane_id.map(String::as_str))
            });
        let startup = {
            let mut agent_restore = AgentRestoreState {
                enabled: runtime_context.resume_agents_on_restore,
                resumed_sessions: resumed_agent_sessions,
            };
            pane_restore_startup(saved_agent_session, saved_history, &mut agent_restore)
        };
        let restored_agent_session =
            restored_terminal_agent_session(saved_agent_session, startup.duplicate_agent_session);
        let initial_restore_agent = startup
            .restore_plan
            .as_ref()
            .and_then(|plan| crate::detect::parse_agent_label(&plan.agent));

        let old_pane_id = reverse_id_map.get(id).copied();
        let public_pane_id = old_pane_id
            .and_then(|old_id| public_pane_ids_by_old_raw.get(&old_id))
            .map(String::as_str);
        let launch_env = public_pane_id
            .map(|pane_id| {
                PaneLaunchEnv::from_extra(Vec::new()).with_identity(
                    workspace_id.to_string(),
                    crate::workspace::public_tab_id_for_number(workspace_id, number),
                    pane_id.to_string(),
                )
            })
            .unwrap_or_default();
        let imported_runtime = old_pane_id.and_then(|old_id| imported_panes.remove(&old_id));
        let was_imported = imported_runtime.is_some();
        #[cfg(unix)]
        let imported_agent_activity = imported_runtime
            .as_ref()
            .and_then(|imported| imported.state.agent_activity.clone());
        #[cfg(unix)]
        let imported_agent_state = imported_runtime
            .as_ref()
            .and_then(|imported| imported.state.agent_state.clone());
        #[cfg(unix)]
        let imported_pane_seen = imported_runtime
            .as_ref()
            .and_then(|imported| imported.state.pane_seen);
        #[cfg(unix)]
        let imported_pane_done_for_ms = imported_runtime
            .as_ref()
            .and_then(|imported| imported.state.pane_done_for_ms);
        let pending_native_agent_restore = if was_imported {
            None
        } else {
            startup.restore_plan.clone()
        };
        if let Some(plan) = pending_native_agent_restore {
            let terminal_id = TerminalId::alloc();
            let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone())
                .with_pending_agent_resume_plan(plan);
            if let Err(error) = terminal.restore_work_context_with_tiers(
                saved_work_context.clone(),
                saved_work_context_tiers.clone(),
            ) {
                warn!(pane_id = id.raw(), %error, "ignoring invalid saved pane work context");
            }
            if let Some(label) = saved_label {
                terminal.set_manual_label(label);
            }
            if let Some(session) = restored_agent_session {
                terminal.set_persisted_agent_session(session);
                restore_auto_settle_state(&mut terminal, saved_agent_session);
            }
            if let Some(report) = saved_pane.and_then(|pane| pane.closing_report.clone()) {
                terminal
                    .restore_closing_report_persistence_state(report, std::time::Instant::now());
            }
            match (saved_agent_name, saved_managed_agent) {
                (Some(agent_name), Some(agent)) => {
                    terminal.restore_managed_agent(agent_name, agent)
                }
                (Some(_), None) => {}
                (None, _) => {}
            }
            if let Some(agent) = initial_restore_agent {
                let _ = terminal.set_detected_state_with_screen_signals_at(
                    Some(agent),
                    AgentState::Idle,
                    false,
                    false,
                    false,
                    false,
                    false,
                    std::time::Instant::now(),
                );
            }
            if initial_restore_agent.is_some() && !startup.duplicate_agent_session {
                reseed_blocked_hook_authority(&mut terminal, saved_agent_session);
            }
            terminal.settled_auto_label =
                saved_pane.and_then(|pane| pane.settled_auto_label.clone());
            let mut pane = PaneState::new(terminal_id);
            pane.group_membership = saved_pane
                .map(|pane| pane.group_membership.clone())
                .unwrap_or_default();
            pane.settled_at = saved_pane.and_then(|pane| pane.settled_at);
            pane.settled_locked = saved_pane.is_some_and(|pane| pane.settled_locked);
            pane.set_snoozed_until(saved_pane.and_then(|pane| pane.snoozed_until));
            pane.settled_work_key = saved_pane.and_then(|pane| pane.settled_work_key.clone());
            restore_pane_activity(&mut pane, saved_pane);
            panes.insert(*id, pane);
            terminals.push(terminal);
            continue;
        }

        #[cfg(not(unix))]
        if imported_runtime.is_some() {
            failed_imports += 1;
            continue;
        }

        let runtime_result = {
            #[cfg(unix)]
            if let Some(imported) = imported_runtime {
                TerminalRuntime::from_handoff_fd(
                    crate::handoff_runtime::ImportedHandoffRuntime {
                        master_fd: imported.master_fd,
                        state: imported.state.with_pane_id(*id),
                    },
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            } else {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }

            #[cfg(not(unix))]
            {
                TerminalRuntime::spawn_with_initial_history(
                    *id,
                    rows,
                    cols,
                    cwd.clone(),
                    runtime_context.scrollback_limit_bytes,
                    crate::terminal_theme::TerminalTheme::default(),
                    None,
                    runtime_context.shell_config,
                    &launch_env,
                    startup.initial_history_ansi,
                    runtime_context.events.clone(),
                    runtime_context.render_notify.clone(),
                    runtime_context.render_dirty.clone(),
                )
            }
        };

        match runtime_result {
            Ok(runtime) => {
                let terminal_id = TerminalId::alloc();
                let mut terminal = TerminalState::new(terminal_id.clone(), cwd.clone());
                if let Err(error) = terminal.restore_work_context_with_tiers(
                    saved_work_context.clone(),
                    saved_work_context_tiers.clone(),
                ) {
                    warn!(pane_id = id.raw(), %error, "ignoring invalid saved pane work context");
                }
                if was_imported {
                    if let Some(argv) = saved_launch_argv {
                        terminal = terminal.with_launch_argv(argv).with_respawn_shell_on_exit();
                    }
                }
                if let Some(label) = saved_label {
                    terminal.set_manual_label(label);
                }
                if let Some(session) = restored_agent_session {
                    terminal.set_persisted_agent_session(session);
                    restore_auto_settle_state(&mut terminal, saved_agent_session);
                }
                if let Some(report) = saved_pane.and_then(|pane| pane.closing_report.clone()) {
                    terminal.restore_closing_report_persistence_state(
                        report,
                        std::time::Instant::now(),
                    );
                }
                match (saved_agent_name, saved_managed_agent) {
                    (Some(agent_name), Some(agent)) if was_imported => {
                        terminal.restore_managed_agent(agent_name, agent)
                    }
                    (Some(_), Some(_)) => {}
                    (Some(agent_name), None) if was_imported => terminal.set_agent_name(agent_name),
                    (Some(_), None) => {}
                    (None, _) => {}
                }
                if let Some(agent) = initial_restore_agent {
                    let _ = terminal.set_detected_state_with_screen_signals_at(
                        Some(agent),
                        AgentState::Idle,
                        false,
                        false,
                        false,
                        false,
                        false,
                        std::time::Instant::now(),
                    );
                }
                if initial_restore_agent.is_some() && !startup.duplicate_agent_session {
                    reseed_blocked_hook_authority(&mut terminal, saved_agent_session);
                }
                terminal.settled_auto_label =
                    saved_pane.and_then(|pane| pane.settled_auto_label.clone());
                #[cfg(unix)]
                {
                    if let Some(activity) = imported_agent_activity {
                        terminal.restore_agent_activity_handoff_state(
                            activity,
                            std::time::Instant::now(),
                        );
                    }
                    if let Some(agent_state) = imported_agent_state {
                        terminal.restore_terminal_agent_handoff_state(
                            agent_state,
                            std::time::Instant::now(),
                        );
                    }
                }
                let mut pane = PaneState::new(terminal_id.clone());
                pane.group_membership = saved_pane
                    .map(|pane| pane.group_membership.clone())
                    .unwrap_or_default();
                pane.settled_at = saved_pane.and_then(|pane| pane.settled_at);
                pane.settled_locked = saved_pane.is_some_and(|pane| pane.settled_locked);
                pane.set_snoozed_until(saved_pane.and_then(|pane| pane.snoozed_until));
                pane.settled_work_key = saved_pane.and_then(|pane| pane.settled_work_key.clone());
                restore_pane_activity(&mut pane, saved_pane);
                #[cfg(unix)]
                let pane = {
                    let mut pane = pane;
                    if let Some(seen) = imported_pane_seen {
                        pane.seen = seen;
                    }
                    pane.done_since = imported_pane_done_for_ms.and_then(|done_for_ms| {
                        std::time::Instant::now()
                            .checked_sub(std::time::Duration::from_millis(done_for_ms))
                    });
                    pane
                };
                panes.insert(*id, pane);
                terminal_runtimes.insert(terminal_id, runtime);
                terminals.push(terminal);
            }
            Err(e) => {
                if let Some(key) = startup.reserved_agent_session.as_deref() {
                    resumed_agent_sessions.remove(key);
                }
                if was_imported {
                    failed_imports += 1;
                    error!(
                        tab = ?snap.custom_name,
                        pane_id = id.raw(),
                        err = %e,
                        "failed to restore imported pane"
                    );
                }
                error!(
                    tab = ?snap.custom_name,
                    pane_id = id.raw(),
                    err = %e,
                    "failed to restore pane"
                );
                if !was_imported {
                    let terminal = unavailable_restored_terminal(
                        saved_pane, cwd,
                        format!("Could not start the saved shell: {e}. Fix the shell configuration and restart this session."),
                    );
                    let mut pane = PaneState::new(terminal.id.clone());
                    restore_pane_activity(&mut pane, saved_pane);
                    panes.insert(*id, pane);
                    terminals.push(terminal);
                }
            }
        }
    }

    if panes.is_empty() {
        warn!(
            tab = ?snap.custom_name,
            "no panes could be restored for tab, dropping it"
        );
        return (None, failed_imports);
    }

    let surviving: HashSet<PaneId> = panes.keys().copied().collect();
    let Some(node) = prune_restored_node(node, &surviving) else {
        warn!(
            tab = ?snap.custom_name,
            "restored tab lost all panes after pruning missing layout nodes"
        );
        return (None, failed_imports);
    };
    let pane_ids = collect_pane_ids(&node);
    let Some(focus) = resolve_restored_pane(snap.focused, &id_map, &surviving, &pane_ids) else {
        return (None, failed_imports);
    };
    let Some(root_pane) = resolve_restored_pane(snap.root_pane, &id_map, &surviving, &pane_ids)
    else {
        return (None, failed_imports);
    };
    let layout = TileLayout::from_saved(node, focus);

    (
        Some((
            crate::workspace::Tab {
                custom_name: snap.custom_name.clone(),
                name_origin: snap.name_origin,
                number,
                root_pane,
                layout,
                panes,
                #[cfg(test)]
                runtimes: HashMap::new(),
                zoomed: snap.zoomed,
                prio: snap.prio,
                pinned: snap.pinned,
                starred: snap.starred,
                subgroup: snap.subgroup.clone(),
                events: runtime_context.events.clone(),
                render_notify: runtime_context.render_notify.clone(),
                render_dirty: runtime_context.render_dirty.clone(),
            },
            terminals,
            terminal_runtimes,
            reverse_id_map,
        )),
        failed_imports,
    )
}

pub(super) fn compatible_history<'a>(
    snapshot: &SessionSnapshot,
    history: Option<&'a SessionHistorySnapshot>,
) -> Option<&'a SessionHistorySnapshot> {
    let history = history?;
    match (
        snapshot.generation.as_deref(),
        history.generation.as_deref(),
    ) {
        (Some(session), Some(saved_history)) if !session.is_empty() && session == saved_history => {
            Some(history)
        }
        (None, None) => {
            warn!(
                "legacy session history has no verifiable generation; restoring topology without history"
            );
            None
        }
        _ => {
            warn!(
                session_generation = ?snapshot.generation,
                history_generation = ?history.generation,
                "session history generation mismatch; restoring topology without history"
            );
            None
        }
    }
}

fn history_for_pane<'a>(
    history: Option<&'a PaneHistorySnapshot>,
    expected_pane_id: Option<&str>,
) -> Option<&'a PaneHistorySnapshot> {
    let history = history?;
    match (history.pane_id.as_deref(), expected_pane_id) {
        (Some(saved), Some(expected)) if !saved.is_empty() && saved == expected => Some(history),
        _ => {
            warn!(
                saved_pane_id = ?history.pane_id,
                expected_pane_id,
                "session pane history identity mismatch; dropping pane history"
            );
            None
        }
    }
}

fn pane_restore_startup<'a>(
    session: Option<&PaneAgentSessionSnapshot>,
    history: Option<&'a PaneHistorySnapshot>,
    agent_restore: &mut AgentRestoreState<'_>,
) -> PaneRestoreStartup<'a> {
    // Native agent resume owns the conversation history. If a pane has a
    // resumable agent session and resume is enabled, do not replay saved pane
    // presentation history into that terminal, even when this pane is a
    // duplicate suppressed by session de-duplication.
    let restore_plan =
        session.and_then(|session| restore_plan_for_snapshot(session, agent_restore.enabled));
    let has_native_agent_restore = restore_plan.is_some();
    // Reserve before spawning so later panes in the same restore pass cannot
    // launch the same native agent session. The caller rolls this reservation
    // back if runtime spawn fails before any agent process is started.
    let mut reserved_agent_session = None;
    let duplicate_agent_session = restore_plan.as_ref().is_some_and(|plan| {
        if agent_restore
            .resumed_sessions
            .insert(plan.dedupe_key.clone())
        {
            reserved_agent_session = Some(plan.dedupe_key.clone());
            false
        } else {
            true
        }
    });
    let restore_plan = if duplicate_agent_session {
        None
    } else {
        restore_plan
    };

    PaneRestoreStartup {
        restore_plan,
        initial_history_ansi: if has_native_agent_restore {
            None
        } else {
            history.map(|history| history.ansi.as_str())
        },
        duplicate_agent_session,
        reserved_agent_session,
    }
}

fn restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
) -> Option<crate::agent_resume::AgentResumePlan> {
    if !resume_agents_on_restore {
        return None;
    }
    let persisted = persisted_agent_session_from_snapshot(session)?;
    crate::agent_resume::plan(&session.source, &session.agent, &persisted.session_ref)
}

/// Put a saved `Blocked` report back on a pane whose agent is being resumed.
///
/// Restore otherwise seeds every resumed agent as `Idle`, which is right for a
/// pane that will speak for itself: the screen re-derives Idle and Working
/// almost immediately. A blocked agent is the exception, because it is waiting
/// on a human and emits nothing until answered, so without this the pane comes
/// back looking finished.
///
/// The report is stored, not trusted: `hook_authority_is_effective` still
/// requires the agent process to be present in the pane, so a resume that never
/// produces the agent shows no state here either.
fn reseed_blocked_hook_authority(
    terminal: &mut TerminalState,
    session: Option<&PaneAgentSessionSnapshot>,
) {
    let Some(session) = session else {
        return;
    };
    let Some(blocked) = session.blocked.as_ref() else {
        return;
    };
    let Some(persisted) = persisted_agent_session_from_snapshot(session) else {
        return;
    };
    // A hook report is only routed onto a pane whose session identity is already
    // anchored, so anchor it here rather than depending on the order of the
    // caller's other restore steps.
    if terminal.persisted_agent_session.is_none() {
        terminal.set_persisted_agent_session(persisted.clone());
    }
    let session_ref = Some(persisted.session_ref);
    terminal.set_hook_authority_report_at(
        session.source.clone(),
        session.agent.clone(),
        AgentState::Blocked,
        blocked.message.clone(),
        None,
        None,
        None,
        session_ref,
        None,
        std::time::Instant::now(),
    );
}

fn persisted_agent_session_from_snapshot(
    session: &PaneAgentSessionSnapshot,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    crate::agent_resume::session_ref_from_snapshot(
        &session.source,
        &session.agent,
        session.kind,
        &session.value,
    )
}

fn restored_terminal_agent_session(
    session: Option<&PaneAgentSessionSnapshot>,
    duplicate_agent_session: bool,
) -> Option<crate::agent_resume::PersistedAgentSession> {
    if duplicate_agent_session {
        return None;
    }
    session.and_then(persisted_agent_session_from_snapshot)
}

#[cfg(test)]
fn take_restore_plan_for_snapshot(
    session: &PaneAgentSessionSnapshot,
    resume_agents_on_restore: bool,
    resumed_agent_sessions: &mut HashSet<String>,
) -> Option<crate::agent_resume::AgentResumePlan> {
    restore_plan_for_snapshot(session, resume_agents_on_restore)
        .filter(|plan| resumed_agent_sessions.insert(plan.dedupe_key.clone()))
}

pub(super) fn prune_restored_node(node: Node, surviving: &HashSet<PaneId>) -> Option<Node> {
    match node {
        Node::Pane(id) => surviving.contains(&id).then_some(Node::Pane(id)),
        Node::Split {
            direction,
            leading,
            ratio,
            first,
            second,
        } => {
            let first = prune_restored_node(*first, surviving);
            let second = prune_restored_node(*second, surviving);
            match (first, second) {
                (Some(first), Some(second)) => Some(Node::Split {
                    direction,
                    leading,
                    ratio,
                    first: Box::new(first),
                    second: Box::new(second),
                }),
                (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
                (None, None) => None,
            }
        }
    }
}

pub(super) fn resolve_restored_pane(
    saved_old_id: Option<u32>,
    id_map: &HashMap<u32, PaneId>,
    surviving: &HashSet<PaneId>,
    pane_ids: &[PaneId],
) -> Option<PaneId> {
    saved_old_id
        .and_then(|old_id| id_map.get(&old_id).copied())
        .filter(|pane_id| surviving.contains(pane_id))
        .or_else(|| pane_ids.first().copied())
}

/// Restore a layout tree, remapping every pane ID to a fresh globally unique one.
/// Returns the new tree and a map of old_raw_id → new PaneId.
pub(super) fn restore_node_remapped(snap: &LayoutSnapshot) -> (Node, HashMap<u32, PaneId>) {
    let mut id_map = HashMap::new();
    let node = remap_inner(snap, &mut id_map);
    (node, id_map)
}

fn remap_inner(snap: &LayoutSnapshot, id_map: &mut HashMap<u32, PaneId>) -> Node {
    match snap {
        LayoutSnapshot::Pane(old_id) => {
            let new_id = PaneId::alloc();
            id_map.insert(*old_id, new_id);
            Node::Pane(new_id)
        }
        LayoutSnapshot::Split {
            direction,
            leading,
            ratio,
            first,
            second,
        } => {
            let first_node = remap_inner(first, id_map);
            let second_node = remap_inner(second, id_map);
            let dir = match direction {
                DirectionSnapshot::Horizontal => Direction::Horizontal,
                DirectionSnapshot::Vertical => Direction::Vertical,
            };
            Node::Split {
                direction: dir,
                leading: *leading,
                ratio: *ratio,
                first: Box::new(first_node),
                second: Box::new(second_node),
            }
        }
    }
}

pub(super) fn collect_pane_ids(node: &Node) -> Vec<PaneId> {
    let mut ids = Vec::new();
    collect_ids_inner(node, &mut ids);
    ids
}

fn collect_ids_inner(node: &Node, ids: &mut Vec<PaneId>) {
    match node {
        Node::Pane(id) => ids.push(*id),
        Node::Split { first, second, .. } => {
            collect_ids_inner(first, ids);
            collect_ids_inner(second, ids);
        }
    }
}

#[cfg(test)]
mod tests {
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    use super::*;

    #[test]
    fn restore_pane_activity_restores_distinct_output_and_input_clocks() {
        let now_unix = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs();
        let saved = super::super::snapshot::PaneSnapshot {
            cwd: "/tmp".into(),
            is_companion: true,
            last_activity_at: Some(now_unix.saturating_sub(60)),
            detection_output_at: Some(now_unix.saturating_sub(120)),
            quiet_since_at: Some(now_unix.saturating_sub(60)),
            settled_at: None,
            settled_locked: false,
            snoozed_until: None,
            settled_work_key: None,
            settled_auto_label: None,
            work_context: Default::default(),
            work_context_tiers: None,
            label: None,
            agent_name: None,
            managed_agent_kind: None,
            closing_report: None,
            agent_session: None,
            group_membership: Default::default(),
            launch_argv: None,
        };
        let mut pane = PaneState::new(TerminalId::alloc());

        restore_pane_activity(&mut pane, Some(&saved));

        assert!(pane.is_companion);
        let quiet_for = pane.activity.quiet_for(Instant::now()).unwrap();
        assert!(quiet_for >= Duration::from_secs(60));
        assert!(quiet_for < Duration::from_secs(65));
        let output_for =
            Instant::now().saturating_duration_since(pane.activity.detection_output_at().unwrap());
        assert!(output_for >= Duration::from_secs(120));
        assert!(output_for < Duration::from_secs(125));
        assert!(pane.activity.inactive_for(Instant::now()) < output_for);
    }

    fn agent_session_snapshot(
        blocked: Option<super::super::snapshot::PaneAgentBlockedSnapshot>,
    ) -> super::super::snapshot::PaneAgentSessionSnapshot {
        super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "pi-session".into(),
            blocked,
            user_replied: false,
            auto_settle_turn_seq: None,
        }
    }

    /// A pane that came back with its agent must come back blocked, because the
    /// agent is waiting on a human and will say nothing more until answered.
    #[test]
    fn a_saved_blocked_report_is_put_back_on_the_restored_pane() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_detected_state(Some(crate::detect::Agent::Pi), AgentState::Idle);
        let session =
            agent_session_snapshot(Some(super::super::snapshot::PaneAgentBlockedSnapshot {
                message: Some("needs a decision".into()),
            }));

        reseed_blocked_hook_authority(&mut terminal, Some(&session));

        let authority = terminal
            .hook_authority
            .as_ref()
            .expect("a saved blocked report should be reinstated");
        assert_eq!(authority.state, AgentState::Blocked);
        assert_eq!(authority.message.as_deref(), Some("needs a decision"));
        assert_eq!(authority.source, "herdr:pi");
        assert_eq!(authority.agent_label, "pi");
    }

    /// The session identity has to come back with the report, or the next real
    /// report from the agent is treated as a different session.
    #[test]
    fn a_reinstated_blocked_report_keeps_its_session_identity() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_detected_state(Some(crate::detect::Agent::Pi), AgentState::Idle);
        let session =
            agent_session_snapshot(Some(super::super::snapshot::PaneAgentBlockedSnapshot {
                message: None,
            }));

        reseed_blocked_hook_authority(&mut terminal, Some(&session));

        let session_ref = terminal
            .hook_authority
            .as_ref()
            .and_then(|authority| authority.session_ref.as_ref())
            .expect("the reinstated report should carry the saved session");
        assert_eq!(session_ref.value, "pi-session");
    }

    /// Everything that was not blocked stays unclaimed, so live screen detection
    /// is what decides the pane's state.
    #[test]
    fn a_session_saved_without_a_blocked_report_claims_nothing() {
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_detected_state(Some(crate::detect::Agent::Pi), AgentState::Idle);

        reseed_blocked_hook_authority(&mut terminal, Some(&agent_session_snapshot(None)));
        reseed_blocked_hook_authority(&mut terminal, None);

        assert!(terminal.hook_authority.is_none());
    }

    #[test]
    fn ac3_generation_mismatch_drops_all_history() {
        let snapshot = SessionSnapshot {
            generation: Some("topology-generation".into()),
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: Vec::new(),
            active: None,
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: HashSet::new(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let history = SessionHistorySnapshot {
            generation: Some("history-generation".into()),
            layout_fingerprint: None,
            version: super::super::snapshot::SNAPSHOT_VERSION,
            workspaces: Vec::new(),
        };

        assert!(compatible_history(&snapshot, Some(&history)).is_none());
    }

    #[test]
    fn ac3_wrong_pane_identity_drops_history_instead_of_reattaching() {
        let history = PaneHistorySnapshot {
            pane_id: Some("workspace:p2".into()),
            ansi: "private output".into(),
            lines: 1,
        };

        assert!(history_for_pane(Some(&history), Some("workspace:p1")).is_none());
    }

    #[test]
    fn ac4_legacy_history_without_identity_is_not_confidentiality_safe() {
        let history = PaneHistorySnapshot {
            pane_id: None,
            ansi: "legacy output".into(),
            lines: 1,
        };

        assert!(history_for_pane(Some(&history), Some("workspace:p1")).is_none());
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    #[cfg(windows)]
    fn test_restore_shell() -> &'static str {
        "C:\\Windows\\System32\\whoami.exe"
    }

    #[cfg(not(windows))]
    fn test_restore_shell() -> &'static str {
        "/bin/sh"
    }

    #[test]
    fn capture_and_restore_node_round_trip() {
        let node = Node::Split {
            direction: Direction::Horizontal,
            leading: true,
            ratio: 0.5,
            first: Box::new(Node::Pane(PaneId::from_raw(0))),
            second: Box::new(Node::Split {
                direction: Direction::Vertical,
                leading: true,
                ratio: 0.3,
                first: Box::new(Node::Pane(PaneId::from_raw(1))),
                second: Box::new(Node::Pane(PaneId::from_raw(2))),
            }),
        };

        let snap = super::super::snapshot::capture_node(&node);
        let (restored, id_map) = restore_node_remapped(&snap);

        assert!(matches!(snap, LayoutSnapshot::Split { leading: true, .. }));
        assert!(matches!(&restored, Node::Split { leading: true, .. }));
        assert_eq!(id_map.len(), 3);
        let ids = collect_pane_ids(&restored);
        assert_eq!(ids.len(), 3);
        let unique: std::collections::HashSet<u32> = ids.iter().map(|id| id.raw()).collect();
        assert_eq!(unique.len(), 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn pane_quiet_clock_survives_cold_restore_and_live_handoff() {
        let mut state = crate::app::AppState::test_new();
        state.workspaces = vec![Workspace::test_new("quiet-clock")];
        state.ensure_test_terminals();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&pane_id]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .expect("root terminal")
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("quiet-clock")
                    .expect("valid session id"),
            });
        let quiet_for = std::time::Duration::from_secs(31 * 60);
        let quiet_since = std::time::Instant::now() - quiet_for;
        let pane = state.workspaces[0].tabs[0]
            .panes
            .get_mut(&pane_id)
            .expect("root pane");
        pane.activity.set_last_at(quiet_since);
        pane.activity.observe_quiet(true, quiet_since);
        let snapshot = crate::persist::capture(
            &state.workspaces,
            &state.terminals,
            &Default::default(),
            None,
            0,
            26,
            0.5,
            Default::default(),
            false,
            crate::config::WindowCycleModeConfig::default(),
            false,
        );
        let encoded = serde_json::to_string(&snapshot).expect("encode snapshot");
        let decoded: SessionSnapshot = serde_json::from_str(&encoded).expect("decode snapshot");
        let (events, _events_rx) = mpsc::channel(4);
        let (cold_workspaces, _cold_terminals, _cold_runtimes) = restore(
            &decoded,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );
        let cold_pane = &cold_workspaces[0].tabs[0].panes[&cold_workspaces[0].tabs[0].root_pane];
        assert!(
            cold_pane
                .activity
                .quiet_for(std::time::Instant::now())
                .is_some_and(|age| age >= quiet_for),
            "cold restore reset the quiet clock"
        );

        let mut imports = HashMap::new();
        let (handoff_workspaces, _handoff_terminals, handoff_runtimes) = restore_handoff(
            &decoded,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            &mut imports,
            mpsc::channel(4).0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .expect("live handoff restore");
        let handoff_pane =
            &handoff_workspaces[0].tabs[0].panes[&handoff_workspaces[0].tabs[0].root_pane];
        assert!(
            handoff_pane
                .activity
                .quiet_for(std::time::Instant::now())
                .is_some_and(|age| age >= quiet_for),
            "live handoff reset the quiet clock"
        );
        assert!(handoff_runtimes.is_empty());
    }

    #[test]
    fn prune_restored_node_collapses_missing_branch() {
        let keep = PaneId::from_raw(11);
        let missing = PaneId::from_raw(12);
        let node = Node::Split {
            direction: Direction::Horizontal,
            leading: false,
            ratio: 0.5,
            first: Box::new(Node::Pane(keep)),
            second: Box::new(Node::Pane(missing)),
        };
        let surviving = std::collections::HashSet::from([keep]);

        let pruned = prune_restored_node(node, &surviving).expect("remaining pane should survive");

        assert!(matches!(pruned, Node::Pane(id) if id == keep));
    }

    #[test]
    fn resolve_restored_pane_prefers_surviving_saved_id_and_falls_back_to_first_remaining() {
        let first = PaneId::from_raw(21);
        let second = PaneId::from_raw(22);
        let id_map = HashMap::from([(0_u32, first), (1_u32, second)]);
        let surviving = std::collections::HashSet::from([first]);
        let pane_ids = vec![first];

        assert_eq!(
            resolve_restored_pane(Some(0), &id_map, &surviving, &pane_ids),
            Some(first)
        );
        assert_eq!(
            resolve_restored_pane(Some(1), &id_map, &surviving, &pane_ids),
            Some(first)
        );
    }

    #[test]
    fn restored_worktree_space_membership_drops_missing_checkout() {
        let missing =
            std::env::temp_dir().join(format!("herdr-missing-worktree-{}", std::process::id()));
        let membership = crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: missing.join("repo"),
            checkout_path: missing.join("checkout"),
            is_linked_worktree: true,
        };

        assert_eq!(restored_worktree_space_membership(Some(membership)), None);
    }

    #[test]
    fn restore_plan_respects_opt_in_and_allowlist() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };

        assert!(restore_plan_for_snapshot(&session, false).is_none());
        assert_eq!(
            restore_plan_for_snapshot(&session, true).unwrap().argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );

        let unsupported_path = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:claude".into(),
            agent: "claude".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("claude-session"),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        assert!(restore_plan_for_snapshot(&unsupported_path, true).is_none());
    }

    #[test]
    fn restore_plan_selection_suppresses_duplicates() {
        let pi_session_path = test_session_path("pi-session.jsonl");
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: pi_session_path.clone(),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        let mut resumed = HashSet::new();

        assert!(take_restore_plan_for_snapshot(&session, false, &mut resumed).is_none());
        assert!(resumed.is_empty());

        let first = take_restore_plan_for_snapshot(&session, true, &mut resumed)
            .expect("first restore should get a plan");
        assert_eq!(
            first.argv,
            vec!["pi", "--session", pi_session_path.as_str()]
        );
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            pane_id: None,
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_some());
        assert!(startup.initial_history_ansi.is_none());
        assert!(!startup.duplicate_agent_session);
    }

    #[test]
    fn pane_restore_startup_suppresses_history_for_duplicate_native_agent_session() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            pane_id: None,
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: true,
            resumed_sessions: &mut resumed,
        };

        let first = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);
        let duplicate = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(first.restore_plan.is_some());
        assert!(first.initial_history_ansi.is_none());
        assert!(duplicate.restore_plan.is_none());
        assert!(duplicate.initial_history_ansi.is_none());
        assert!(duplicate.duplicate_agent_session);
    }

    #[test]
    fn pane_restore_startup_keeps_history_without_native_agent_resume() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        let history = super::super::snapshot::PaneHistorySnapshot {
            pane_id: None,
            ansi: "RESTORED_HISTORY\r\n".into(),
            lines: 1,
        };
        let mut resumed = HashSet::new();
        let mut agent_restore = AgentRestoreState {
            enabled: false,
            resumed_sessions: &mut resumed,
        };

        let startup = pane_restore_startup(Some(&session), Some(&history), &mut agent_restore);

        assert!(startup.restore_plan.is_none());
        assert_eq!(startup.initial_history_ansi, Some("RESTORED_HISTORY\r\n"));
        assert!(!startup.duplicate_agent_session);
        assert!(resumed.is_empty());
    }

    #[test]
    fn restore_rehydrates_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:hermes".into(),
            agent: "hermes".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "hermes-session".into(),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };

        let preserved = restored_terminal_agent_session(Some(&session), false)
            .expect("restore should preserve metadata");
        assert_eq!(preserved.source, "herdr:hermes");
        assert_eq!(preserved.agent, "hermes");
        assert_eq!(preserved.session_ref.value, "hermes-session");
    }

    #[test]
    fn restore_rehydrates_auto_settle_reply_and_consumed_turn() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:codex".into(),
            agent: "codex".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Id,
            value: "codex-session".into(),
            blocked: None,
            user_replied: true,
            auto_settle_turn_seq: Some(7),
        };
        let mut terminal = TerminalState::new(TerminalId::alloc(), "/tmp".into());
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: session.source.clone(),
            agent: session.agent.clone(),
            session_ref: crate::agent_resume::AgentSessionRef::id(&session.value).unwrap(),
        });

        restore_auto_settle_state(&mut terminal, Some(&session));
        terminal.set_closing_report_scope(
            "herdr:codex".into(),
            Some("codex-session".into()),
            Some(7),
        );
        assert!(!terminal.observe_auto_settle_transition(true, true, false));
        terminal.set_closing_report_scope(
            "herdr:codex".into(),
            Some("codex-session".into()),
            Some(8),
        );
        assert!(terminal.observe_auto_settle_transition(true, true, false));
    }

    #[test]
    fn restore_does_not_rehydrate_duplicate_agent_session_metadata() {
        let session = super::super::snapshot::PaneAgentSessionSnapshot {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            kind: crate::agent_resume::AgentSessionRefKind::Path,
            value: test_session_path("pi-session.jsonl"),
            blocked: None,
            user_replied: false,
            auto_settle_turn_seq: None,
        };
        let mut resumed = HashSet::new();
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_some());
        assert!(take_restore_plan_for_snapshot(&session, true, &mut resumed).is_none());

        assert!(restored_terminal_agent_session(Some(&session), true).is_none());
    }

    #[tokio::test]
    async fn failed_cold_restore_preserves_panes_and_saved_directories() {
        for missing_shell in [false, true] {
            let mut snapshot: SessionSnapshot = serde_json::from_str(include_str!(
                "../../tests/fixtures/session/current-herdr-session.json"
            ))
            .unwrap();
            let cwd = std::env::current_dir().unwrap();
            let missing = cwd.join("__herdr_missing_restore_directory__");
            assert!(!missing.exists());
            for workspace in &mut snapshot.workspaces {
                workspace.identity_cwd = cwd.clone();
                for tab in &mut workspace.tabs {
                    for pane in tab.panes.values_mut() {
                        pane.cwd = cwd.clone();
                    }
                }
            }
            let failed = snapshot.workspaces[0].tabs[0].panes.get_mut(&1).unwrap();
            failed.cwd = missing.clone();
            failed.label = Some("keep my pane".into());
            failed.agent_session = Some(super::super::snapshot::PaneAgentSessionSnapshot {
                source: "herdr:opencode".into(),
                agent: "opencode".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "keep-my-session".into(),
                blocked: None,
                user_replied: false,
                auto_settle_turn_seq: None,
            });
            let (events, _rx) = mpsc::channel(32);
            let (workspaces, terminals, runtimes) = restore(
                &snapshot,
                None,
                24,
                80,
                0,
                if missing_shell {
                    "__herdr_missing_restore_shell__"
                } else {
                    test_restore_shell()
                },
                crate::config::ShellModeConfig::NonLogin,
                false,
                events,
                Arc::new(Notify::new()),
                Arc::new(RenderSignal::new()),
            );
            let runtimes = crate::terminal::TerminalRuntimeRegistry::from(runtimes);
            let captured = crate::persist::capture(
                &workspaces,
                &terminals,
                &runtimes,
                Some(0),
                0,
                26,
                0.5,
                Default::default(),
                false,
                crate::config::WindowCycleModeConfig::default(),
                false,
            );
            assert_eq!(
                captured.workspaces.len(),
                2,
                "a launch failure must not delete a workspace"
            );
            assert_eq!(captured.workspaces[0].tabs.len(), 2);
            let pane = captured.workspaces[0].tabs[0]
                .panes
                .values()
                .next()
                .unwrap();
            assert_eq!(
                pane.cwd, missing,
                "fallback cwd must not replace saved intent"
            );
            assert_eq!(pane.label.as_deref(), Some("keep my pane"));
            assert_eq!(
                pane.agent_session.as_ref().unwrap().value,
                "keep-my-session"
            );
            let root = workspaces[0].tabs[0].root_pane;
            let terminal_id = workspaces[0].tabs[0].terminal_id(root).unwrap();
            assert!(
                runtimes.get(terminal_id).is_none(),
                "do not open a replacement shell elsewhere"
            );
            let healthy = workspaces[1].tabs[0]
                .terminal_id(workspaces[1].tabs[0].root_pane)
                .unwrap();
            assert_eq!(runtimes.get(healthy).is_some(), !missing_shell);
            assert!(terminals[terminal_id].restore_error.is_some());
            let mut state = crate::app::AppState::test_new();
            state.workspaces = workspaces;
            state.terminals = terminals;
            state.active = Some(0);
            state.assert_invariants_for_test();
        }
    }

    #[tokio::test]
    async fn ac3_restore_carries_persisted_agent_session_and_work_context() {
        let snoozed_until = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap_or_default()
            .as_secs()
            .saturating_add(300);
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    name_origin: Default::default(),
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            cwd,
                            is_companion: false,
                            last_activity_at: None,
                            detection_output_at: None,
                            quiet_since_at: None,
                            settled_at: None,
                            settled_locked: false,
                            snoozed_until: Some(snoozed_until),
                            settled_work_key: None,
                            settled_auto_label: Some("#3 Restore context".into()),
                            work_context: crate::work_context::PaneWorkContext {
                                repo: None,
                                ticket_ids: vec!["MAT-12".into()],
                                pr_urls: vec!["https://github.com/o/r/pull/3".into()],
                                preview_urls: Vec::new(),
                                missive_urls: vec![
                                    "https://mail.missiveapp.com/#inbox/conversations/c1".into(),
                                ],
                                branch: Some("feat/context".into()),
                                work_title: Some("Restore context".into()),
                                session_name: None,
                                role: None,
                                active_owner: false,
                            },
                            work_context_tiers: None,
                            label: Some("reviewer".into()),
                            agent_name: Some("reviewer".into()),
                            managed_agent_kind: Some("opencode".into()),
                            closing_report: None,
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "herdr:opencode".into(),
                                agent: "opencode".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "opencode-session".into(),
                                blocked: None,
                                user_replied: false,
                                auto_settle_turn_seq: None,
                            }),
                            group_membership: Default::default(),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    prio: false,
                    pinned: false,
                    starred: false,
                    subgroup: None,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let terminal = terminals
            .values()
            .next()
            .expect("restored terminal should exist");
        assert_eq!(
            workspaces[0].tabs[0]
                .panes
                .values()
                .next()
                .unwrap()
                .snoozed_until(),
            Some(snoozed_until)
        );
        assert_eq!(
            terminal.settled_auto_label.as_deref(),
            Some("#3 Restore context")
        );
        assert!(
            !terminal.respawn_shell_on_exit,
            "agent sessions should not use native restore lifecycle when resume_agents_on_restore is disabled"
        );
        assert_eq!(terminal.agent_name, None);
        assert_eq!(terminal.manual_label.as_deref(), Some("reviewer"));
        assert_eq!(
            terminal.effective_work_context(),
            &crate::work_context::PaneWorkContext {
                repo: None,
                ticket_ids: vec!["MAT-12".into()],
                pr_urls: vec!["https://github.com/o/r/pull/3".into()],
                preview_urls: Vec::new(),
                missive_urls: vec!["https://mail.missiveapp.com/#inbox/conversations/c1".into(),],
                branch: Some("feat/context".into()),
                work_title: Some("Restore context".into()),
                session_name: None,
                role: None,
                active_owner: false,
            }
        );
        let session = terminal
            .persisted_agent_session
            .as_ref()
            .expect("persisted agent session should survive restore");
        assert_eq!(session.source, "herdr:opencode");
        assert_eq!(session.agent, "opencode");
        assert_eq!(session.session_ref.value, "opencode-session");
    }

    #[tokio::test]
    async fn restore_preserves_public_id_mapping_after_pane_id_remap() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::from([(10, 1), (20, 3)]),
                next_public_pane_number: 4,
                public_tab_numbers: vec![5],
                next_public_tab_number: 6,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    name_origin: Default::default(),
                    layout: LayoutSnapshot::Split {
                        direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                        leading: false,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(10)),
                        second: Box::new(LayoutSnapshot::Pane(20)),
                    },
                    panes: HashMap::from([
                        (
                            10,
                            super::super::snapshot::PaneSnapshot {
                                cwd: cwd.clone(),
                                is_companion: false,
                                last_activity_at: None,
                                detection_output_at: None,
                                quiet_since_at: None,
                                settled_at: None,
                                settled_locked: false,
                                snoozed_until: None,
                                settled_work_key: None,
                                settled_auto_label: None,
                                work_context: Default::default(),
                                work_context_tiers: None,
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                closing_report: None,
                                agent_session: None,
                                group_membership: Default::default(),
                                launch_argv: None,
                            },
                        ),
                        (
                            20,
                            super::super::snapshot::PaneSnapshot {
                                cwd: cwd.clone(),
                                is_companion: false,
                                last_activity_at: None,
                                detection_output_at: None,
                                quiet_since_at: None,
                                settled_at: None,
                                settled_locked: false,
                                snoozed_until: None,
                                settled_work_key: None,
                                settled_auto_label: None,
                                work_context: Default::default(),
                                work_context_tiers: None,
                                label: None,
                                agent_name: None,
                                managed_agent_kind: None,
                                closing_report: None,
                                agent_session: None,
                                group_membership: Default::default(),
                                launch_argv: None,
                            },
                        ),
                    ]),
                    zoomed: false,
                    prio: false,
                    pinned: false,
                    starred: false,
                    subgroup: None,
                    focused: Some(10),
                    root_pane: Some(10),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, _terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let workspace = workspaces.first().expect("workspace should restore");
        let mut public_numbers: Vec<_> = workspace.public_pane_numbers.values().copied().collect();
        public_numbers.sort_unstable();
        assert_eq!(public_numbers, vec![1, 3]);
        assert_eq!(workspace.next_public_pane_number, 4);
        assert_eq!(workspace.tabs[0].number, 5);
        assert_eq!(workspace.next_public_tab_number, 6);
    }

    #[tokio::test]
    async fn cold_restore_with_gapped_public_tab_numbers_drops_unmanaged_agent_name() {
        let cwd = std::env::current_dir().unwrap();
        let pane_snap = |id: &str| {
            (
                id.parse::<u32>().unwrap(),
                super::super::snapshot::PaneSnapshot {
                    cwd: cwd.clone(),
                    is_companion: false,
                    last_activity_at: None,
                    detection_output_at: None,
                    quiet_since_at: None,
                    settled_at: None,
                    settled_locked: false,
                    snoozed_until: None,
                    settled_work_key: None,
                    settled_auto_label: None,
                    work_context: Default::default(),
                    work_context_tiers: None,
                    label: None,
                    agent_name: None,
                    managed_agent_kind: None,
                    closing_report: None,
                    agent_session: None,
                    group_membership: Default::default(),
                    launch_argv: None,
                },
            )
        };
        let final_pane = super::super::snapshot::PaneSnapshot {
            cwd: cwd.clone(),
            is_companion: false,
            last_activity_at: None,
            detection_output_at: None,
            quiet_since_at: None,
            settled_at: None,
            settled_locked: false,
            snoozed_until: None,
            settled_work_key: None,
            settled_auto_label: None,
            work_context: Default::default(),
            work_context_tiers: None,
            label: Some("planner".into()),
            agent_name: Some("planner".into()),
            managed_agent_kind: None,
            closing_report: None,
            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                source: "herdr:codex".into(),
                agent: "codex".into(),
                kind: crate::agent_resume::AgentSessionRefKind::Id,
                value: "codex-session".into(),
                blocked: None,
                user_replied: false,
                auto_settle_turn_seq: None,
            }),
            group_membership: Default::default(),
            launch_argv: None,
        };
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("w1".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::from([(10, 1), (11, 2), (12, 3), (13, 4)]),
                next_public_pane_number: 5,
                public_tab_numbers: vec![1, 3, 4, 5],
                next_public_tab_number: 6,
                tabs: vec![
                    TabSnapshot {
                        custom_name: None,
                        name_origin: Default::default(),
                        layout: LayoutSnapshot::Pane(10),
                        panes: HashMap::from([pane_snap("10")]),
                        zoomed: false,
                        prio: false,
                        pinned: false,
                        starred: false,
                        subgroup: None,
                        focused: Some(10),
                        root_pane: Some(10),
                    },
                    TabSnapshot {
                        custom_name: None,
                        name_origin: Default::default(),
                        layout: LayoutSnapshot::Pane(11),
                        panes: HashMap::from([pane_snap("11")]),
                        zoomed: false,
                        prio: false,
                        pinned: false,
                        starred: false,
                        subgroup: None,
                        focused: Some(11),
                        root_pane: Some(11),
                    },
                    TabSnapshot {
                        custom_name: None,
                        name_origin: Default::default(),
                        layout: LayoutSnapshot::Pane(12),
                        panes: HashMap::from([pane_snap("12")]),
                        zoomed: false,
                        prio: false,
                        pinned: false,
                        starred: false,
                        subgroup: None,
                        focused: Some(12),
                        root_pane: Some(12),
                    },
                    TabSnapshot {
                        custom_name: None,
                        name_origin: Default::default(),
                        layout: LayoutSnapshot::Pane(13),
                        panes: HashMap::from([(13, final_pane)]),
                        zoomed: false,
                        prio: false,
                        pinned: false,
                        starred: false,
                        subgroup: None,
                        focused: Some(13),
                        root_pane: Some(13),
                    },
                ],
                active_tab: 3,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (workspaces, terminals, _runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let workspace = workspaces.first().expect("workspace should restore");
        assert_eq!(workspace.active_tab, 3);
        assert_eq!(workspace.tabs[3].number, 5);
        let agent_pane = workspace.tabs[3].root_pane;
        let terminal_id = &workspace.tabs[3].panes[&agent_pane].attached_terminal_id;
        assert!(terminals[terminal_id].agent_name.is_none());
        assert_eq!(terminals[terminal_id].managed_agent_kind(), None);
        let restored_detail = workspace
            .pane_details(&terminals)
            .into_iter()
            .find(|detail| detail.pane_id == agent_pane)
            .expect("agentless panes remain visible in the final ownership projection");
        assert!(restored_detail.agent.is_none());
    }

    #[test]
    fn legacy_restore_precomputes_missing_public_pane_numbers() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = WorkspaceSnapshot {
            is_fleet: false,
            repo_binding_cleared: false,
            id: Some("w1".into()),
            custom_name: None,
            identity_cwd: cwd,
            worktree_space: None,
            repo_binding: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![TabSnapshot {
                custom_name: None,
                name_origin: Default::default(),
                layout: LayoutSnapshot::Split {
                    direction: super::super::snapshot::DirectionSnapshot::Horizontal,
                    leading: false,
                    ratio: 0.5,
                    first: Box::new(LayoutSnapshot::Pane(10)),
                    second: Box::new(LayoutSnapshot::Pane(20)),
                },
                panes: HashMap::new(),
                zoomed: false,
                prio: false,
                pinned: false,
                starred: false,
                subgroup: None,
                focused: Some(10),
                root_pane: Some(10),
            }],
            active_tab: 0,
        };
        let mut next_public_pane_number = 1;

        let public_numbers =
            migrated_public_pane_numbers_by_old_raw(&snapshot, &mut next_public_pane_number);

        assert_eq!(public_numbers, HashMap::from([(10, 1), (20, 2)]));
        assert_eq!(next_public_pane_number, 3);
    }

    #[tokio::test]
    #[cfg(unix)]
    async fn native_agent_restore_defers_runtime_launch() {
        let cwd = std::env::current_dir().unwrap();
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd.clone(),
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    name_origin: Default::default(),
                    layout: LayoutSnapshot::Pane(0),
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneSnapshot {
                            cwd,
                            is_companion: false,
                            last_activity_at: None,
                            detection_output_at: None,
                            quiet_since_at: None,
                            settled_at: None,
                            settled_locked: false,
                            snoozed_until: None,
                            settled_work_key: None,
                            settled_auto_label: None,
                            work_context: Default::default(),
                            work_context_tiers: None,
                            label: None,
                            agent_name: None,
                            managed_agent_kind: None,
                            closing_report: None,
                            agent_session: Some(super::super::snapshot::PaneAgentSessionSnapshot {
                                source: "herdr:codex".into(),
                                agent: "codex".into(),
                                kind: crate::agent_resume::AgentSessionRefKind::Id,
                                value: "codex-session".into(),
                                blocked: None,
                                user_replied: false,
                                auto_settle_turn_seq: None,
                            }),
                            group_membership: Default::default(),
                            launch_argv: None,
                        },
                    )]),
                    zoomed: false,
                    prio: false,
                    pinned: false,
                    starred: false,
                    subgroup: None,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let (events, _event_rx) = mpsc::channel(4);

        let (_workspaces, terminals, runtimes) = restore(
            &snapshot,
            None,
            24,
            80,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            true,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let terminal = terminals
            .values()
            .next()
            .expect("native agent restore should create terminal state");
        assert!(
            terminal.pending_agent_resume_plan.is_some(),
            "restored native agent panes should defer resume until client terminal context is known"
        );
        assert!(
            !terminal.respawn_shell_on_exit,
            "deferred agent resume should not use native restore lifecycle before launch"
        );
        assert!(
            runtimes.is_empty(),
            "native agent restore should not spawn a fallback-size runtime during snapshot restore"
        );
        let mut imports = HashMap::new();
        let (_handoff_workspaces, handoff_terminals, handoff_runtimes) = restore_handoff(
            &snapshot,
            0,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            &mut imports,
            mpsc::channel(4).0,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        )
        .expect("handoff restore should preserve pending native agent resume");
        let handoff_terminal = handoff_terminals
            .values()
            .next()
            .expect("handoff restore should create terminal state");
        assert!(
            handoff_terminal.pending_agent_resume_plan.is_some(),
            "handoff restore should preserve pending native agent resume intent"
        );
        assert!(
            handoff_runtimes.is_empty(),
            "handoff restore should not replace pending native agent resume with a shell runtime"
        );
    }

    #[tokio::test]
    async fn restore_seeds_saved_pane_history_into_runtime() {
        let (snapshot, history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            Some(&history),
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        );
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        let restored_text = runtime.recent_unwrapped_text(10);
        assert!(
            restored_text.contains("RESTORED_HISTORY 👨‍👩‍👧 LINK"),
            "styled Unicode and hyperlink text should survive history replay"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    #[tokio::test]
    async fn restore_without_history_snapshot_keeps_pane_contents_empty() {
        let (snapshot, _history) = snapshot_with_saved_pane_history();
        let (events, _events_rx) = mpsc::channel(8);
        let render_notify = Arc::new(Notify::new());
        let render_dirty = Arc::new(RenderSignal::new());

        let (_workspaces, _terminals, runtimes) = restore(
            &snapshot,
            None,
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            render_notify,
            render_dirty,
        );
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");

        assert!(
            !runtime
                .recent_unwrapped_text(10)
                .contains("RESTORED_HISTORY"),
            "pane history should not restore unless a history snapshot is supplied"
        );

        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(2);
        while runtime.cwd().is_none() && std::time::Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(10));
        }
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
    }

    #[tokio::test]
    async fn ac3_matching_generation_with_wrong_pane_identity_drops_secret() {
        let (snapshot, mut history) = snapshot_with_saved_pane_history();
        history.workspaces[0].tabs[0]
            .panes
            .get_mut(&0)
            .unwrap()
            .pane_id = Some("workspace:p2".into());

        let (workspaces, restored_text) = restore_fixture_text(&snapshot, Some(&history));

        assert_eq!(workspaces.len(), 1);
        assert!(!restored_text.contains("RESTORED_HISTORY"));
    }

    #[tokio::test]
    async fn ac2_mismatched_generation_restores_topology_without_history() {
        let (snapshot, mut history) = snapshot_with_saved_pane_history();
        history.generation = Some("crash-generation".into());

        let (workspaces, restored_text) = restore_fixture_text(&snapshot, Some(&history));

        assert_eq!(workspaces.len(), 1);
        assert!(!restored_text.contains("RESTORED_HISTORY"));
    }

    #[tokio::test]
    async fn ac4_legacy_v3_restores_topology_but_drops_unverifiable_history() {
        let (mut snapshot, mut history) = snapshot_with_saved_pane_history();
        snapshot.version = 3;
        snapshot.generation = None;
        history.version = 3;
        history.generation = None;
        history.workspaces[0].tabs[0]
            .panes
            .get_mut(&0)
            .unwrap()
            .pane_id = None;

        let (workspaces, restored_text) = restore_fixture_text(&snapshot, Some(&history));

        assert_eq!(workspaces.len(), 1);
        assert!(!restored_text.contains("RESTORED_HISTORY"));
    }

    fn restore_fixture_text(
        snapshot: &SessionSnapshot,
        history: Option<&SessionHistorySnapshot>,
    ) -> (Vec<Workspace>, String) {
        let (events, _events_rx) = mpsc::channel(8);
        let (workspaces, _terminals, runtimes) = restore(
            snapshot,
            history,
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );
        let runtime = runtimes
            .values()
            .next()
            .expect("restored runtime should exist");
        let text = runtime.recent_unwrapped_text(10);
        let _ = runtime.try_send_bytes(bytes::Bytes::from_static(b"exit\n"));
        (workspaces, text)
    }

    #[tokio::test]
    async fn fleet_workspace_ac5_restore_keeps_fleet_out_of_upper_workspace_list() {
        let (mut snapshot, history) = snapshot_with_saved_pane_history();
        snapshot.workspaces[0].is_fleet = true;

        let (workspaces, _) = restore_fixture_text(&snapshot, Some(&history));
        assert!(workspaces[0].is_fleet);
        let mut state = crate::app::AppState::test_new();
        state.workspaces = workspaces;
        state.active = Some(0);
        state.selected = 0;

        assert!(crate::ui::sidebar::workspace_list_entries(&state).is_empty());
    }

    #[tokio::test]
    async fn session_restore_keeps_pane_payload_with_its_public_identity() {
        let (mut snapshot, _) = snapshot_with_saved_pane_history();
        let workspace = &mut snapshot.workspaces[0];
        workspace.public_pane_numbers = HashMap::from([(0, 7)]);
        workspace.next_public_pane_number = 8;
        workspace.tabs[0]
            .panes
            .get_mut(&0)
            .expect("saved pane")
            .label = Some("identity-marker".into());
        let group_id: crate::groups::GroupId = serde_json::from_value(serde_json::json!({
            "owner": "AQEBAQEBAQEBAQEBAQEBAQ",
            "local": 7
        }))
        .expect("group id");
        workspace.tabs[0]
            .panes
            .get_mut(&0)
            .expect("saved pane")
            .group_membership = crate::groups::PaneGroupMembership {
            group_id: Some(group_id.clone()),
            revision: 3,
        };
        let (events, _events_rx) = mpsc::channel(8);

        let (workspaces, terminals, mut runtimes) = restore(
            &snapshot,
            None,
            5,
            40,
            4096,
            test_restore_shell(),
            crate::config::ShellModeConfig::NonLogin,
            false,
            events,
            Arc::new(Notify::new()),
            Arc::new(RenderSignal::new()),
        );

        let workspace = &workspaces[0];
        let (&restored_pane_id, _) = workspace
            .public_pane_numbers
            .iter()
            .find(|(_, number)| **number == 7)
            .expect("saved public pane identity");
        assert_ne!(restored_pane_id.raw(), 0, "internal pane id is remapped");
        let terminal_id = &workspace
            .pane_state(restored_pane_id)
            .expect("restored pane")
            .attached_terminal_id;
        assert_eq!(
            terminals[terminal_id].manual_label.as_deref(),
            Some("identity-marker")
        );
        assert_eq!(
            workspace
                .pane_state(restored_pane_id)
                .expect("restored pane")
                .group_membership,
            crate::groups::PaneGroupMembership {
                group_id: Some(group_id),
                revision: 3,
            }
        );
        for (_, runtime) in runtimes.drain() {
            runtime.shutdown();
        }
    }

    #[tokio::test]
    async fn restore_rejects_history_from_another_layout_or_without_provenance() {
        for legacy in [false, true] {
            let (mut snapshot, history) = snapshot_with_saved_pane_history();
            let mut value = serde_json::to_value(history).unwrap();
            if legacy {
                value.as_object_mut().unwrap().remove("layout_fingerprint");
            } else {
                snapshot.workspaces[0].tabs[0]
                    .panes
                    .get_mut(&0)
                    .unwrap()
                    .cwd = std::env::temp_dir();
            }
            let history = serde_json::from_value(value).unwrap();
            let (events, _rx) = mpsc::channel(8);
            let (_, _, runtimes) = restore(
                &snapshot,
                Some(&history),
                5,
                80,
                4096,
                test_restore_shell(),
                crate::config::ShellModeConfig::NonLogin,
                false,
                events,
                Arc::new(Notify::new()),
                Arc::new(RenderSignal::new()),
            );
            let runtime = runtimes.values().next().unwrap();
            assert!(
                !runtime
                    .recent_unwrapped_text(10)
                    .contains("RESTORED_HISTORY"),
                "screen history must belong to the exact saved layout"
            );
            for (_, runtime) in runtimes {
                runtime.shutdown();
            }
        }
    }

    fn snapshot_with_saved_pane_history() -> (SessionSnapshot, SessionHistorySnapshot) {
        let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
        let mut panes = HashMap::new();
        panes.insert(
            0,
            super::super::snapshot::PaneSnapshot {
                cwd: cwd.clone(),
                is_companion: false,
                last_activity_at: None,
                detection_output_at: None,
                quiet_since_at: None,
                settled_at: None,
                settled_locked: false,
                snoozed_until: None,
                settled_work_key: None,
                settled_auto_label: None,
                work_context: Default::default(),
                work_context_tiers: None,
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                closing_report: None,
                agent_session: None,
                group_membership: Default::default(),
                launch_argv: None,
            },
        );
        let mut history = SessionHistorySnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: Some("saved-generation".into()),
            layout_fingerprint: None,
            workspaces: vec![WorkspaceHistorySnapshot {
                tabs: vec![super::super::snapshot::TabHistorySnapshot {
                    panes: HashMap::from([(
                        0,
                        super::super::snapshot::PaneHistorySnapshot {
                            pane_id: Some("workspace:p1".into()),
                            ansi: concat!(
                                "\x1b[31mRESTORED_HISTORY 👨‍👩‍👧\x1b[0m ",
                                "\x1b]8;;https://example.com\x1b\\LINK\x1b]8;;\x1b\\\r\n"
                            )
                            .to_string(),
                            lines: 1,
                        },
                    )]),
                }],
            }],
        };
        let snapshot = SessionSnapshot {
            version: super::super::snapshot::SNAPSHOT_VERSION,
            generation: Some("saved-generation".into()),
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("workspace".into()),
                custom_name: None,
                identity_cwd: cwd,
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::new(),
                next_public_pane_number: 0,
                public_tab_numbers: Vec::new(),
                next_public_tab_number: 0,
                tabs: vec![TabSnapshot {
                    custom_name: None,
                    name_origin: Default::default(),
                    layout: LayoutSnapshot::Pane(0),
                    panes,
                    zoomed: false,
                    prio: false,
                    pinned: false,
                    starred: false,
                    subgroup: None,
                    focused: Some(0),
                    root_pane: Some(0),
                }],
                active_tab: 0,
            }],
            active: Some(0),
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: Default::default(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        history.layout_fingerprint = super::super::snapshot::layout_fingerprint(&snapshot);
        (snapshot, history)
    }
}
