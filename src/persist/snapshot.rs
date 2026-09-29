use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::time::{Instant, SystemTime, UNIX_EPOCH};

use ratatui::layout::Direction;
use serde::{Deserialize, Serialize};

use crate::layout::Node;
use crate::terminal::{TerminalRuntime, TerminalRuntimeRegistry};
use crate::workspace::Workspace;

fn is_false(value: &bool) -> bool {
    !*value
}

/// Current snapshot format version.
pub(super) const SNAPSHOT_VERSION: u32 = 4;

/// Serializable snapshot of the entire herdr session.
#[derive(Serialize, Deserialize)]
pub struct SessionSnapshot {
    /// Format version — used to detect incompatible changes.
    #[serde(default)]
    pub version: u32,
    /// Commit generation shared with the matching history snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    pub workspaces: Vec<WorkspaceSnapshot>,
    pub active: Option<usize>,
    pub selected: usize,
    #[serde(default)]
    pub sidebar_width: Option<u16>,
    #[serde(default)]
    pub sidebar_section_split: Option<f32>,
    #[serde(default)]
    pub collapsed_space_keys: std::collections::HashSet<String>,
    #[serde(default)]
    pub prio_panel_collapsed: bool,
    #[serde(default, skip_serializing_if = "is_default_window_cycle_mode")]
    pub window_cycle_mode: Option<crate::config::WindowCycleModeConfig>,
    #[serde(default, skip_serializing_if = "is_default_skip_collapsed_cycle")]
    pub skip_collapsed_cycle: Option<bool>,
}

fn is_default_window_cycle_mode(value: &Option<crate::config::WindowCycleModeConfig>) -> bool {
    value.is_none_or(|mode| mode == crate::config::WindowCycleModeConfig::ThisMachine)
}

fn is_default_skip_collapsed_cycle(value: &Option<bool>) -> bool {
    value.is_none_or(|skip| !skip)
}

#[derive(Serialize, Deserialize)]
pub struct SessionHistorySnapshot {
    /// Format version follows the matching session snapshot version.
    #[serde(default)]
    pub version: u32,
    /// Commit generation shared with the matching topology snapshot.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub generation: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub layout_fingerprint: Option<String>,
    pub workspaces: Vec<WorkspaceHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceHistorySnapshot {
    pub tabs: Vec<TabHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct TabHistorySnapshot {
    pub panes: HashMap<u32, PaneHistorySnapshot>,
}

#[derive(Serialize, Deserialize)]
pub struct WorkspaceSnapshot {
    #[serde(default)]
    pub id: Option<String>,
    /// Fleet workspaces were added after the snapshot format existed.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub is_fleet: bool,
    #[serde(default)]
    pub custom_name: Option<String>,
    pub identity_cwd: PathBuf,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub worktree_space: Option<crate::workspace::WorktreeSpaceMembership>,
    /// Absent in session files written before repository binding existed, so
    /// it defaults rather than forcing a snapshot-version migration.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub repo_binding: Option<String>,
    /// Absent in session files written before adoption existed; an old session
    /// has never been cleared, so `false` is the correct default.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub repo_binding_cleared: bool,
    #[serde(default)]
    pub public_pane_numbers: HashMap<u32, usize>,
    #[serde(default)]
    pub next_public_pane_number: usize,
    #[serde(default)]
    pub public_tab_numbers: Vec<usize>,
    #[serde(default)]
    pub next_public_tab_number: usize,
    pub tabs: Vec<TabSnapshot>,
    #[serde(default)]
    pub active_tab: usize,
}

#[derive(Deserialize)]
struct LegacyWorkspaceSnapshot {
    #[serde(default)]
    custom_name: Option<String>,
    layout: LayoutSnapshot,
    panes: HashMap<u32, PaneSnapshot>,
    zoomed: bool,
    #[serde(default)]
    focused: Option<u32>,
    #[serde(default)]
    root_pane: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct TabSnapshot {
    #[serde(default)]
    pub custom_name: Option<String>,
    #[serde(default)]
    pub name_origin: crate::workspace::TabNameOrigin,
    pub layout: LayoutSnapshot,
    pub panes: HashMap<u32, PaneSnapshot>,
    pub zoomed: bool,
    /// User-set: this tab is pinned into the sidebar's own group. Defaulted so
    /// session files written before pins existed restore unchanged.
    #[serde(default)]
    pub pinned: bool,
    /// User-set focus tag rendered as a star. Defaulted so session files written
    /// before stars existed restore unchanged.
    #[serde(default)]
    pub starred: bool,
    /// User-named sidebar subgroup. Defaulted so session files written before
    /// subgroups existed restore unchanged.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub subgroup: Option<String>,
    #[serde(default)]
    pub prio: bool,
    #[serde(default)]
    pub focused: Option<u32>,
    #[serde(default)]
    pub root_pane: Option<u32>,
}

#[derive(Serialize, Deserialize)]
pub struct PaneSnapshot {
    pub cwd: PathBuf,
    #[serde(default, skip_serializing_if = "is_false")]
    pub is_companion: bool,
    #[serde(
        default,
        skip_serializing_if = "crate::groups::PaneGroupMembership::is_default"
    )]
    pub group_membership: crate::groups::PaneGroupMembership,
    /// Unix timestamp of the pane's last meaningful activity.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub last_activity_at: Option<u64>,
    /// Unix timestamp of the latest changed detection-buffer output.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detection_output_at: Option<u64>,
    /// Unix timestamp of the latest observed not-quiet to quiet transition.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub quiet_since_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_at: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub snoozed_until: Option<u64>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_work_key: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub settled_auto_label: Option<String>,
    /// Flat effective projection, kept for readers of older formats.
    #[serde(default)]
    pub work_context: crate::work_context::PaneWorkContext,
    /// Per-tier provenance (manual/hook/git/restored). When absent (legacy
    /// snapshots), the flat `work_context` restores as a replaceable fallback.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub work_context_tiers: Option<crate::work_context::PaneWorkContextTiers>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub label: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_name: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub managed_agent_kind: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub agent_session: Option<PaneAgentSessionSnapshot>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub launch_argv: Option<Vec<String>>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentSessionSnapshot {
    pub source: String,
    pub agent: String,
    pub kind: crate::agent_resume::AgentSessionRefKind,
    pub value: String,
    /// The blocked report that was live when the server stopped.
    ///
    /// Only `Blocked` is carried across a restart. Idle and Working are
    /// re-derived from the screen within a frame of the pane coming back, but a
    /// blocked agent says nothing further until a human answers it, so dropping
    /// the report loses the one state that was waiting on someone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub blocked: Option<PaneAgentBlockedSnapshot>,
}

/// The parts of a blocked hook report that outlive the process that made it.
///
/// Only the message: a declared `wait`/`eta_s` describes work in flight and is
/// dropped for any report that is not `Working`, so a blocked report never has
/// one to carry.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PaneAgentBlockedSnapshot {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub message: Option<String>,
}

#[derive(Serialize, Deserialize)]
pub struct PaneHistorySnapshot {
    /// Stable public pane identity; absent in legacy v3 history files.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub pane_id: Option<String>,
    pub ansi: String,
    pub lines: usize,
}

/// Serializable BSP tree.
#[derive(Serialize, Deserialize)]
pub enum LayoutSnapshot {
    Pane(u32),
    Split {
        direction: DirectionSnapshot,
        #[serde(default)]
        leading: bool,
        ratio: f32,
        first: Box<LayoutSnapshot>,
        second: Box<LayoutSnapshot>,
    },
}

#[derive(Serialize, Deserialize)]
pub enum DirectionSnapshot {
    Horizontal,
    Vertical,
}

impl From<LegacyWorkspaceSnapshot> for WorkspaceSnapshot {
    fn from(snap: LegacyWorkspaceSnapshot) -> Self {
        let identity_cwd = legacy_identity_cwd(&snap);
        let tab = TabSnapshot {
            custom_name: None,
            name_origin: Default::default(),
            layout: snap.layout,
            panes: snap.panes,
            zoomed: snap.zoomed,
            prio: false,
            pinned: false,
            starred: false,
            subgroup: None,
            focused: snap.focused,
            root_pane: snap.root_pane,
        };

        Self {
            id: None,
            is_fleet: false,
            custom_name: snap.custom_name,
            identity_cwd,
            worktree_space: None,
            repo_binding_cleared: false,
            repo_binding: None,
            public_pane_numbers: HashMap::new(),
            next_public_pane_number: 0,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs: vec![tab],
            active_tab: 0,
        }
    }
}

#[derive(Deserialize)]
struct RawSessionSnapshot {
    #[serde(default)]
    version: u32,
    #[serde(default)]
    generation: Option<String>,
    #[serde(default)]
    workspaces: Vec<serde_json::Value>,
    #[serde(default)]
    active: Option<usize>,
    #[serde(default)]
    selected: usize,
    #[serde(default)]
    sidebar_width: Option<u16>,
    #[serde(default)]
    sidebar_section_split: Option<f32>,
    #[serde(default)]
    collapsed_space_keys: std::collections::HashSet<String>,
    #[serde(default)]
    prio_panel_collapsed: bool,
    #[serde(default)]
    window_cycle_mode: Option<crate::config::WindowCycleModeConfig>,
    #[serde(default)]
    skip_collapsed_cycle: Option<bool>,
}

fn migrate_snapshot(raw: RawSessionSnapshot) -> Result<SessionSnapshot, String> {
    Ok(SessionSnapshot {
        version: raw.version,
        generation: raw.generation,
        workspaces: raw
            .workspaces
            .into_iter()
            .map(migrate_workspace)
            .collect::<Result<Vec<_>, _>>()?,
        active: raw.active,
        selected: raw.selected,
        sidebar_width: raw.sidebar_width,
        sidebar_section_split: raw.sidebar_section_split,
        collapsed_space_keys: raw.collapsed_space_keys,
        prio_panel_collapsed: raw.prio_panel_collapsed,
        window_cycle_mode: raw.window_cycle_mode,
        skip_collapsed_cycle: raw.skip_collapsed_cycle,
    })
}

fn migrate_workspace(raw: serde_json::Value) -> Result<WorkspaceSnapshot, String> {
    if raw.get("identity_cwd").is_some() {
        return serde_json::from_value(raw).map_err(|e| e.to_string());
    }

    if raw.get("layout").is_some() {
        let legacy =
            serde_json::from_value::<LegacyWorkspaceSnapshot>(raw).map_err(|e| e.to_string())?;
        return Ok(legacy.into());
    }

    Err("workspace snapshot is neither current nor legacy format".to_string())
}

fn legacy_identity_cwd(snap: &LegacyWorkspaceSnapshot) -> PathBuf {
    let root_pane = snap
        .root_pane
        .or_else(|| first_pane_id_in_layout(&snap.layout));

    root_pane
        .and_then(|pane_id| snap.panes.get(&pane_id))
        .map(|pane| pane.cwd.clone())
        .or_else(|| {
            first_pane_id_in_layout(&snap.layout)
                .and_then(|pane_id| snap.panes.get(&pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .or_else(|| {
            snap.panes
                .keys()
                .min()
                .and_then(|pane_id| snap.panes.get(pane_id))
                .map(|pane| pane.cwd.clone())
        })
        .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()))
}

fn first_pane_id_in_layout(layout: &LayoutSnapshot) -> Option<u32> {
    match layout {
        LayoutSnapshot::Pane(id) => Some(*id),
        LayoutSnapshot::Split { first, second, .. } => {
            first_pane_id_in_layout(first).or_else(|| first_pane_id_in_layout(second))
        }
    }
}

/// Capture the current app state into a serializable snapshot.
pub fn capture(
    workspaces: &[Workspace],
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
    active: Option<usize>,
    selected: usize,
    sidebar_width: u16,
    sidebar_section_split: f32,
    collapsed_space_keys: std::collections::HashSet<String>,
    prio_panel_collapsed: bool,
    window_cycle_mode: crate::config::WindowCycleModeConfig,
    skip_collapsed_cycle: bool,
) -> SessionSnapshot {
    let captured_at = Instant::now();
    let captured_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SessionSnapshot {
        version: SNAPSHOT_VERSION,
        generation: None,
        workspaces: workspaces
            .iter()
            .map(|workspace| {
                capture_workspace(
                    workspace,
                    terminals,
                    terminal_runtimes,
                    captured_at,
                    captured_at_unix,
                )
            })
            .collect(),
        active,
        selected,
        sidebar_width: Some(sidebar_width),
        sidebar_section_split: Some(sidebar_section_split),
        collapsed_space_keys,
        prio_panel_collapsed,
        window_cycle_mode: Some(window_cycle_mode),
        skip_collapsed_cycle: Some(skip_collapsed_cycle),
    }
}

fn capture_workspace(
    ws: &Workspace,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
    captured_at: Instant,
    captured_at_unix: u64,
) -> WorkspaceSnapshot {
    let captured_tabs = capture_tabs(
        ws,
        terminals,
        terminal_runtimes,
        false,
        captured_at,
        captured_at_unix,
    );
    let identity_cwd = captured_tabs
        .first()
        .and_then(|captured| {
            captured
                .snapshot
                .root_pane
                .and_then(|root_pane| captured.snapshot.panes.get(&root_pane))
                .map(|pane| pane.cwd.clone())
        })
        .unwrap_or_else(|| ws.identity_cwd.clone());
    let public_tab_numbers = captured_tabs
        .iter()
        .map(|captured| captured.tab.number)
        .collect();
    let active_tab = captured_tabs
        .iter()
        .position(|captured| captured.source_index == ws.active_tab)
        .or_else(|| {
            captured_tabs
                .iter()
                .position(|captured| captured.source_index > ws.active_tab)
        })
        .unwrap_or_else(|| captured_tabs.len().saturating_sub(1));
    let tabs = captured_tabs
        .into_iter()
        .map(|captured| captured.snapshot)
        .collect();
    WorkspaceSnapshot {
        id: Some(ws.id.clone()),
        is_fleet: ws.is_fleet,
        custom_name: ws.custom_name.clone(),
        identity_cwd,
        worktree_space: ws.worktree_space.clone(),
        repo_binding: ws.repo_binding.clone(),
        repo_binding_cleared: ws.repo_binding_cleared,
        public_pane_numbers: ws
            .public_pane_numbers
            .iter()
            .filter(|(pane_id, _)| {
                ws.tabs.iter().all(|tab| {
                    tab.panes.get(pane_id).is_none_or(|pane| {
                        !terminal_runtimes
                            .get(&pane.attached_terminal_id)
                            .is_some_and(TerminalRuntime::is_remote_proxy)
                    })
                })
            })
            .map(|(pane_id, number)| (pane_id.raw(), *number))
            .collect(),
        next_public_pane_number: ws.next_public_pane_number,
        public_tab_numbers,
        next_public_tab_number: ws.next_public_tab_number,
        tabs,
        active_tab,
    }
}

struct CapturedTab<'a> {
    source_index: usize,
    tab: &'a crate::workspace::Tab,
    snapshot: TabSnapshot,
    history: Option<HashMap<u32, PaneHistorySnapshot>>,
}

fn capture_tabs<'a>(
    ws: &'a Workspace,
    terminals: &HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
    include_history: bool,
    captured_at: Instant,
    captured_at_unix: u64,
) -> Vec<CapturedTab<'a>> {
    ws.tabs
        .iter()
        .enumerate()
        .filter_map(|(source_index, tab)| {
            let snapshot = capture_tab(
                tab,
                terminals,
                terminal_runtimes,
                captured_at,
                captured_at_unix,
            )?;
            Some(CapturedTab {
                source_index,
                tab,
                snapshot,
                history: include_history.then(|| capture_tab_history(ws, tab, terminal_runtimes)),
            })
        })
        .collect()
}

fn capture_tab(
    tab: &crate::workspace::Tab,
    terminals: &std::collections::HashMap<
        crate::terminal::TerminalId,
        crate::terminal::TerminalState,
    >,
    terminal_runtimes: &TerminalRuntimeRegistry,
    captured_at: Instant,
    captured_at_unix: u64,
) -> Option<TabSnapshot> {
    let excluded_panes = tab
        .panes
        .iter()
        .filter_map(|(pane_id, pane)| {
            terminal_runtimes
                .get(&pane.attached_terminal_id)
                .is_some_and(TerminalRuntime::is_remote_proxy)
                .then_some(*pane_id)
        })
        .collect::<HashSet<_>>();
    let layout = capture_node_without_panes(tab.layout.root(), &excluded_panes)?;
    let mut panes = HashMap::new();
    for id in tab.panes.keys() {
        if excluded_panes.contains(id) {
            continue;
        }
        let terminal_id = tab.terminal_id(*id);
        let cwd = terminal_id
            .and_then(|id| terminal_runtimes.get(id))
            .and_then(TerminalRuntime::cwd_for_persistence)
            .or_else(|| {
                terminal_id
                    .and_then(|id| terminals.get(id))
                    .map(|state| state.cwd.clone())
            })
            .unwrap_or_else(|| std::env::current_dir().unwrap_or_else(|_| "/".into()));
        let pane = tab.panes.get(id);
        let terminal = pane.and_then(|pane| terminals.get(&pane.attached_terminal_id));
        let label = terminal.and_then(|terminal| terminal.manual_label.clone());
        let (agent_name, managed_agent_kind) = terminal
            .filter(|terminal| !terminal.managed_agent_launch_pending())
            .map(|terminal| {
                (
                    terminal.agent_name.clone(),
                    terminal
                        .managed_agent_kind()
                        .map(|agent| crate::detect::agent_label(agent).to_string()),
                )
            })
            .unwrap_or_default();
        let launch_argv = terminal.and_then(|terminal| terminal.launch_argv.clone());
        let work_context = terminal
            .map(|terminal| terminal.effective_work_context().clone())
            .unwrap_or_default();
        let work_context_tiers = terminal.map(|terminal| terminal.work_context.snapshot_tiers());
        let agent_session = terminal.and_then(|terminal| {
            if let Some(authority) = terminal.hook_authority.as_ref() {
                if let Some(session_ref) = authority.session_ref.as_ref() {
                    // A retired report no longer speaks for the pane, so it
                    // must not be revived by a restart either.
                    let blocked = (authority.state == crate::detect::AgentState::Blocked
                        && authority.retired_at.is_none())
                    .then(|| PaneAgentBlockedSnapshot {
                        message: authority.message.clone(),
                    });
                    return Some(PaneAgentSessionSnapshot {
                        source: authority.source.clone(),
                        agent: authority.agent_label.clone(),
                        kind: session_ref.kind,
                        value: session_ref.value.clone(),
                        blocked,
                    });
                }
            }
            terminal
                .persisted_agent_session
                .as_ref()
                .map(|session| PaneAgentSessionSnapshot {
                    source: session.source.clone(),
                    agent: session.agent.clone(),
                    kind: session.session_ref.kind,
                    value: session.session_ref.value.clone(),
                    // No live authority, so there is no report to carry.
                    blocked: None,
                })
        });
        panes.insert(
            id.raw(),
            PaneSnapshot {
                cwd,
                is_companion: pane.is_some_and(|pane| pane.is_companion),
                group_membership: pane
                    .map(|pane| pane.group_membership.clone())
                    .unwrap_or_default(),
                last_activity_at: pane.map(|pane| {
                    pane.activity
                        .unix_timestamp_at(captured_at, captured_at_unix)
                }),
                detection_output_at: pane.and_then(|pane| {
                    pane.activity
                        .detection_output_unix_timestamp_at(captured_at, captured_at_unix)
                }),
                quiet_since_at: pane.and_then(|pane| {
                    pane.activity
                        .quiet_unix_timestamp_at(captured_at, captured_at_unix)
                }),
                settled_at: pane.and_then(|pane| pane.settled_at),
                snoozed_until: pane.and_then(crate::pane::PaneState::snoozed_until),
                settled_work_key: pane.and_then(|pane| pane.settled_work_key.clone()),
                settled_auto_label: terminal
                    .and_then(|terminal| terminal.settled_auto_label.clone()),
                work_context,
                work_context_tiers,
                label,
                agent_name,
                managed_agent_kind,
                agent_session,
                launch_argv,
            },
        );
    }
    let focused = if excluded_panes.contains(&tab.layout.focused()) {
        first_pane_id_in_layout(&layout)?
    } else {
        tab.layout.focused().raw()
    };
    let root_pane = first_pane_id_in_layout(&layout)?;
    Some(TabSnapshot {
        custom_name: tab.custom_name.clone(),
        name_origin: tab.name_origin,
        layout,
        panes,
        zoomed: tab.zoomed,
        prio: tab.prio,
        pinned: tab.pinned,
        starred: tab.starred,
        subgroup: tab.subgroup.clone(),
        focused: Some(focused),
        root_pane: Some(root_pane),
    })
}

pub(super) fn layout_fingerprint(snapshot: &SessionSnapshot) -> Option<String> {
    use sha2::{Digest, Sha256};

    let mut value = serde_json::to_value(snapshot).ok()?;
    // The commit generation is injected while topology and history are written.
    // It proves that the two files belong together, but it is not part of the
    // layout captured before that write.
    value.as_object_mut()?.remove("generation");
    // Sets serialize as arrays; normalize their order as well as JSON object keys.
    let mut collapsed: Vec<_> = snapshot.collapsed_space_keys.iter().collect();
    collapsed.sort_unstable();
    value["collapsed_space_keys"] = serde_json::to_value(collapsed).ok()?;
    let bytes = serde_json::to_vec(&value).ok()?;
    Some(format!("{:x}", Sha256::digest(bytes)))
}

/// Capture pane screen history separately from the structural session snapshot.
pub fn capture_history(
    snapshot: &SessionSnapshot,
    workspaces: &[Workspace],
    terminals: &HashMap<crate::terminal::TerminalId, crate::terminal::TerminalState>,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> SessionHistorySnapshot {
    let captured_at = Instant::now();
    let captured_at_unix = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .unwrap_or_default()
        .as_secs();
    SessionHistorySnapshot {
        version: SNAPSHOT_VERSION,
        generation: None,
        layout_fingerprint: layout_fingerprint(snapshot),
        workspaces: workspaces
            .iter()
            .map(|workspace| WorkspaceHistorySnapshot {
                tabs: capture_tabs(
                    workspace,
                    terminals,
                    terminal_runtimes,
                    true,
                    captured_at,
                    captured_at_unix,
                )
                .into_iter()
                .map(|captured| TabHistorySnapshot {
                    panes: captured.history.unwrap_or_default(),
                })
                .collect(),
            })
            .collect(),
    }
}

fn capture_tab_history(
    workspace: &Workspace,
    tab: &crate::workspace::Tab,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> HashMap<u32, PaneHistorySnapshot> {
    let mut panes = HashMap::new();
    for (id, pane) in &tab.panes {
        let pane_id = workspace
            .public_pane_number(*id)
            .map(|number| crate::workspace::public_pane_id_for_number(&workspace.id, number));
        if let Some(history) = capture_pane_history(Some(pane), pane_id, terminal_runtimes) {
            panes.insert(id.raw(), history);
        }
    }
    panes
}

fn capture_pane_history(
    pane: Option<&crate::pane::PaneState>,
    pane_id: Option<String>,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<PaneHistorySnapshot> {
    let runtime = terminal_runtimes.get(&pane?.attached_terminal_id)?;
    if runtime.is_remote_proxy() {
        // Proxy screens belong to the remote terminal, not to local history.
        return None;
    }
    let ansi = runtime.snapshot_history()?;
    let lines = ansi.lines().count();
    Some(PaneHistorySnapshot {
        pane_id,
        ansi,
        lines,
    })
}

#[cfg(test)]
pub(super) fn capture_node(node: &Node) -> LayoutSnapshot {
    match node {
        Node::Pane(id) => LayoutSnapshot::Pane(id.raw()),
        Node::Split {
            direction,
            leading,
            ratio,
            first,
            second,
        } => LayoutSnapshot::Split {
            direction: match direction {
                Direction::Horizontal => DirectionSnapshot::Horizontal,
                Direction::Vertical => DirectionSnapshot::Vertical,
            },
            leading: *leading,
            ratio: *ratio,
            first: Box::new(capture_node(first)),
            second: Box::new(capture_node(second)),
        },
    }
}

fn capture_node_without_panes(
    node: &Node,
    excluded_panes: &HashSet<crate::layout::PaneId>,
) -> Option<LayoutSnapshot> {
    match node {
        Node::Pane(id) => (!excluded_panes.contains(id)).then_some(LayoutSnapshot::Pane(id.raw())),
        Node::Split {
            direction,
            leading,
            ratio,
            first,
            second,
        } => match (
            capture_node_without_panes(first, excluded_panes),
            capture_node_without_panes(second, excluded_panes),
        ) {
            (Some(first), Some(second)) => Some(LayoutSnapshot::Split {
                direction: match direction {
                    Direction::Horizontal => DirectionSnapshot::Horizontal,
                    Direction::Vertical => DirectionSnapshot::Vertical,
                },
                leading: *leading,
                ratio: *ratio,
                first: Box::new(first),
                second: Box::new(second),
            }),
            (Some(remaining), None) | (None, Some(remaining)) => Some(remaining),
            (None, None) => None,
        },
    }
}

pub(super) fn parse_snapshot(content: &str) -> Result<SessionSnapshot, String> {
    let raw = serde_json::from_str::<RawSessionSnapshot>(content).map_err(|e| e.to_string())?;
    if raw.version > SNAPSHOT_VERSION {
        return Err(format!(
            "snapshot version {} is newer than supported {}",
            raw.version, SNAPSHOT_VERSION
        ));
    }
    migrate_snapshot(raw)
}

pub(super) fn parse_history_snapshot(content: &str) -> Result<SessionHistorySnapshot, String> {
    let snapshot =
        serde_json::from_str::<SessionHistorySnapshot>(content).map_err(|e| e.to_string())?;
    if snapshot.version > SNAPSHOT_VERSION {
        return Err(format!(
            "history snapshot version {} is newer than supported {}",
            snapshot.version, SNAPSHOT_VERSION
        ));
    }
    Ok(snapshot)
}

pub(super) fn snapshot_file_version(content: &str) -> Option<u32> {
    #[derive(Deserialize)]
    struct Header {
        version: u32,
    }
    serde_json::from_str::<Header>(content)
        .ok()
        .map(|header| header.version)
}

#[cfg(test)]
mod tests {
    use std::collections::HashMap;
    use std::path::PathBuf;
    use std::time::Duration;

    use ratatui::layout::{Direction, Rect};

    use super::*;
    use crate::app::{AppState, Mode};
    use crate::layout::NavDirection;
    use crate::workspace::Workspace;

    fn session_fixture(name: &str) -> &'static str {
        match name {
            "current-herdr" => {
                include_str!("../../tests/fixtures/session/current-herdr-session.json")
            }
            "current-herdr-dev" => {
                include_str!("../../tests/fixtures/session/current-herdr-dev-session.json")
            }
            "legacy-pre-tabs-v2" => {
                include_str!("../../tests/fixtures/session/legacy-pre-tabs-v2.json")
            }
            other => panic!("unknown session fixture: {other}"),
        }
    }

    fn test_session_path(name: &str) -> String {
        std::env::current_dir()
            .unwrap()
            .join(name)
            .display()
            .to_string()
    }

    fn state_with_workspaces(names: &[&str]) -> AppState {
        let mut state = AppState::test_new();
        state.workspaces = names.iter().map(|name| Workspace::test_new(name)).collect();
        state.ensure_test_terminals();
        if !state.workspaces.is_empty() {
            state.active = Some(0);
            state.selected = 0;
            state.set_server_mode(Mode::Terminal);
        }
        state
    }

    fn capture_from_state(state: &AppState) -> SessionSnapshot {
        let terminal_runtimes = TerminalRuntimeRegistry::new();
        capture_from_state_with_runtimes(state, &terminal_runtimes)
    }

    fn capture_from_state_with_runtimes(
        state: &AppState,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> SessionSnapshot {
        capture(
            &state.workspaces,
            &state.terminals,
            terminal_runtimes,
            state.active,
            state.selected,
            state.sidebar_width,
            state.sidebar_section_split,
            state.collapsed_space_keys.clone(),
            state.prio_panel_collapsed,
            state.window_cycle_mode,
            state.skip_collapsed_cycle,
        )
    }

    #[test]
    fn pane_companion_marker_round_trips_and_defaults_for_legacy_snapshots() {
        let companion: PaneSnapshot = serde_json::from_value(serde_json::json!({
            "cwd": "/tmp",
            "is_companion": true,
        }))
        .unwrap();
        assert!(companion.is_companion);
        assert_eq!(
            serde_json::to_value(&companion).unwrap()["is_companion"],
            true
        );

        let legacy: PaneSnapshot =
            serde_json::from_value(serde_json::json!({ "cwd": "/tmp" })).unwrap();
        assert!(!legacy.is_companion);
    }

    #[test]
    fn agent_state_cache_is_absent_from_persisted_session_snapshot() {
        let mut state = state_with_workspaces(&["runtime-only-agent-state"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        state
            .agent_states
            .report(
                pane_id,
                crate::agent_state::AgentReportPayload {
                    goal: Some(Some("never-persist-this-goal".into())),
                    status_text: Some(Some("never-persist-this-status".into())),
                    ..crate::agent_state::AgentReportPayload::default()
                },
                SystemTime::now(),
            )
            .expect("valid report");

        let encoded = serde_json::to_string(&capture_from_state(&state)).unwrap();
        assert!(!encoded.contains("never-persist-this-goal"));
        assert!(!encoded.contains("never-persist-this-status"));
        assert!(!encoded.contains("agent_states"));
    }

    #[test]
    fn fleet_workspace_ac5_snapshot_persists_marker_and_defaults_old_sessions() {
        let mut state = state_with_workspaces(&["fleet"]);
        state.workspaces[0].is_fleet = true;

        let snapshot = capture_from_state(&state);
        assert!(snapshot.workspaces[0].is_fleet);
        let mut encoded = serde_json::to_value(snapshot).expect("serialize session");
        let workspace = encoded["workspaces"][0]
            .as_object_mut()
            .expect("workspace JSON object");
        assert_eq!(
            workspace.get("is_fleet"),
            Some(&serde_json::Value::Bool(true))
        );

        workspace.remove("is_fleet");
        let legacy: SessionSnapshot =
            serde_json::from_value(encoded).expect("old session without fleet marker loads");
        assert!(!legacy.workspaces[0].is_fleet);
    }

    #[test]
    fn fleet_workspace_ac10_cycle_settings_round_trip_and_default_old_fixture() {
        let mut state = state_with_workspaces(&["cycle-settings"]);
        state.window_cycle_mode = crate::config::WindowCycleModeConfig::ThisMachineAndFleet;
        state.skip_collapsed_cycle = true;

        let snapshot = capture_from_state(&state);
        assert_eq!(
            snapshot.window_cycle_mode,
            Some(crate::config::WindowCycleModeConfig::ThisMachineAndFleet)
        );
        assert_eq!(snapshot.skip_collapsed_cycle, Some(true));
        let encoded = serde_json::to_string(&snapshot).expect("serialize cycle settings");
        let restored = parse_snapshot(&encoded).expect("restore cycle settings");
        assert_eq!(restored.window_cycle_mode, snapshot.window_cycle_mode);
        assert_eq!(restored.skip_collapsed_cycle, Some(true));

        let frozen: SessionSnapshot = serde_json::from_str(include_str!(
            "../../tests/fixtures/session/current-herdr-session.json"
        ))
        .expect("load unchanged frozen session fixture");
        assert_eq!(frozen.window_cycle_mode, None);
        assert_eq!(frozen.skip_collapsed_cycle, None);
    }

    fn capture_history_from_state_with_runtimes(
        state: &AppState,
        terminal_runtimes: &TerminalRuntimeRegistry,
    ) -> SessionHistorySnapshot {
        let snapshot = capture_from_state_with_runtimes(state, terminal_runtimes);
        capture_history(
            &snapshot,
            &state.workspaces,
            &state.terminals,
            terminal_runtimes,
        )
    }

    #[test]
    fn remote_proxy_tabs_are_excluded_from_capture_and_history() {
        let mut state = state_with_workspaces(&["local"]);
        let terminal_runtimes = {
            let mut registry = TerminalRuntimeRegistry::new();
            let pane_id = crate::layout::PaneId::alloc();
            let terminal_id = crate::terminal::TerminalId::alloc();
            let (runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
                pane_id,
                24,
                80,
                0,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
                crate::remote::RemoteFocusOperationState::new(),
            )
            .expect("proxy runtime");
            let terminal = crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
            state.terminals.insert(terminal_id.clone(), terminal);
            registry.insert(terminal_id.clone(), runtime);
            let events = state.workspaces[0].tabs[0].events.clone();
            state.workspaces[0].create_tab_from_existing_pane(
                crate::workspace::MovedPane {
                    pane_id,
                    pane_state: crate::pane::PaneState::new(terminal_id),
                },
                Some("buildbox::w1:p3".to_string()),
                events,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            );
            registry
        };
        state.workspaces[0].active_tab = 1;
        let local_pane = state.workspaces[0].test_split(ratatui::layout::Direction::Horizontal);
        let proxy_tab = state.workspaces[0].active_tab();
        assert_eq!(state.workspaces[0].tabs.len(), 2);
        assert!(proxy_tab.is_some_and(|tab| tab.panes.len() == 2));

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(
            snapshot.workspaces[0].tabs.len(),
            2,
            "a local sibling remains restorable when its tab also contains a proxy"
        );
        let mixed_tab = snapshot.workspaces[0]
            .tabs
            .iter()
            .find(|tab| tab.custom_name.as_deref() == Some("buildbox::w1:p3"))
            .expect("mixed proxy tab remains with its local pane");
        match &mixed_tab.layout {
            LayoutSnapshot::Pane(id) => assert_eq!(*id, local_pane.raw()),
            LayoutSnapshot::Split { .. } => panic!("proxy leaf was not pruned"),
        }
        assert!(mixed_tab.panes.contains_key(&local_pane.raw()));
        assert_eq!(mixed_tab.panes.len(), 1);
        let history = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        assert!(
            history
                .workspaces
                .iter()
                .flat_map(|workspace| workspace.tabs.iter())
                .all(|tab| tab.panes.is_empty()),
            "a proxy screen never enters local pane history"
        );
    }

    #[tokio::test]
    async fn local_proxy_local_capture_restores_tab_identity_focus_and_history() {
        let mut state = state_with_workspaces(&["local"]);
        let (first_terminal, second_tab, second_pane, second_terminal) = {
            let workspace = &mut state.workspaces[0];
            workspace.identity_cwd = PathBuf::from("/local/identity");
            let first_pane = workspace.tabs[0].root_pane;
            let first_terminal = workspace.tabs[0]
                .panes
                .get(&first_pane)
                .expect("first pane")
                .attached_terminal_id
                .clone();
            let second_tab = workspace.test_add_tab(Some("later-local"));
            let second_pane = workspace.tabs[second_tab].root_pane;
            let second_terminal = workspace.tabs[second_tab]
                .panes
                .get(&second_pane)
                .expect("second pane")
                .attached_terminal_id
                .clone();
            (first_terminal, second_tab, second_pane, second_terminal)
        };
        state.ensure_test_terminals();
        state
            .terminals
            .get_mut(&first_terminal)
            .expect("first terminal")
            .cwd = PathBuf::from("/local/one");
        state
            .terminals
            .get_mut(&second_terminal)
            .expect("second terminal")
            .cwd = PathBuf::from("/local/two");

        let proxy_pane = crate::layout::PaneId::alloc();
        let proxy_terminal = crate::terminal::TerminalId::alloc();
        let (proxy_runtime, _channels) = crate::terminal::TerminalRuntime::spawn_remote_proxy(
            proxy_pane,
            24,
            80,
            0,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        state.terminals.insert(
            proxy_terminal.clone(),
            crate::terminal::TerminalState::new(proxy_terminal.clone(), "/remote/proxy".into()),
        );
        let workspace = &mut state.workspaces[0];
        let events = workspace.tabs[0].events.clone();
        let proxy_tab = workspace.create_tab_from_existing_pane(
            crate::workspace::MovedPane {
                pane_id: proxy_pane,
                pane_state: crate::pane::PaneState::new(proxy_terminal.clone()),
            },
            Some("remote-proxy".to_owned()),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
        );
        assert_eq!(proxy_tab, 2);
        workspace.active_tab = second_tab;
        assert!(workspace.move_tab(proxy_tab, 1));
        assert_eq!(workspace.active_tab, 2);

        let first_runtime = crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
            80,
            24,
            0,
            b"LOCAL_ONE\r\n",
        );
        let second_runtime = crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
            80,
            24,
            0,
            b"LOCAL_TWO\r\n",
        );
        let terminal_runtimes = TerminalRuntimeRegistry::from(HashMap::from([
            (first_terminal.clone(), first_runtime),
            (second_terminal.clone(), second_runtime),
            (proxy_terminal, proxy_runtime),
        ]));

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        let captured = &snapshot.workspaces[0];
        assert_eq!(captured.identity_cwd, PathBuf::from("/local/one"));
        assert_eq!(captured.public_tab_numbers, vec![1, 2]);
        assert_eq!(captured.tabs.len(), 2);
        assert_eq!(captured.active_tab, 1);
        assert_eq!(captured.tabs[1].focused, Some(second_pane.raw()));

        assert!(state.workspaces[0].move_tab(1, 0));
        let reordered = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        assert_eq!(
            reordered.workspaces[0].identity_cwd,
            PathBuf::from("/local/one")
        );

        let history = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        assert_eq!(history.workspaces[0].tabs.len(), 2);
        assert!(history.workspaces[0].tabs[0]
            .panes
            .values()
            .any(|pane| pane.ansi.contains("LOCAL_ONE")));
        assert!(history.workspaces[0].tabs[1]
            .panes
            .values()
            .any(|pane| pane.ansi.contains("LOCAL_TWO")));

        let (events, _events_rx) = tokio::sync::mpsc::channel(8);
        let (restored_workspaces, _restored_terminals, mut restored_runtimes) =
            crate::persist::restore(
                &snapshot,
                Some(&history),
                24,
                80,
                0,
                "/bin/sh",
                crate::config::ShellModeConfig::NonLogin,
                false,
                events,
                std::sync::Arc::new(tokio::sync::Notify::new()),
                std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
            );
        let restored = &restored_workspaces[0];
        assert_eq!(
            restored
                .tabs
                .iter()
                .map(|tab| tab.number)
                .collect::<Vec<_>>(),
            vec![1, 2]
        );
        assert_eq!(restored.active_tab, 1);
        assert_eq!(
            restored.tabs[1].layout.focused(),
            restored.tabs[1].root_pane
        );
        for (_terminal_id, runtime) in restored_runtimes.drain() {
            runtime.shutdown();
        }
    }

    fn root_split_ratio(tab: &TabSnapshot) -> Option<f32> {
        match &tab.layout {
            LayoutSnapshot::Split { ratio, .. } => Some(*ratio),
            LayoutSnapshot::Pane(_) => None,
        }
    }

    #[test]
    fn tab_subgroup_round_trips_through_the_session_snapshot() {
        let mut state = state_with_workspaces(&["subgrouped"]);
        state.workspaces[0].tabs[0].set_subgroup(Some(" api ".to_string()));
        let snapshot = capture_from_state(&state);
        assert_eq!(
            snapshot.workspaces[0].tabs[0].subgroup.as_deref(),
            Some("api"),
            "the assignment is captured normalized"
        );
        let json = serde_json::to_string(&snapshot).expect("serialize session snapshot");
        let restored: SessionSnapshot =
            serde_json::from_str(&json).expect("parse session snapshot");
        assert_eq!(
            restored.workspaces[0].tabs[0].subgroup.as_deref(),
            Some("api")
        );
    }

    #[test]
    fn tab_snapshot_defaults_a_missing_subgroup_for_legacy_sessions() {
        let snapshot: TabSnapshot =
            serde_json::from_str(r#"{"layout":{"Pane":1},"panes":{},"zoomed":false}"#)
                .expect("legacy tab snapshot without a subgroup field");
        assert_eq!(snapshot.subgroup, None);
        // An unset subgroup is also omitted when writing, so sessions that
        // never used subgroups keep their old shape.
        let json = serde_json::to_string(&snapshot).expect("serialize tab snapshot");
        assert!(!json.contains("subgroup"), "{json}");
    }

    #[test]
    fn managed_agent_snapshot_omits_pending_and_persists_active_ownership() {
        let mut state = state_with_workspaces(&["managed-snapshot"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let now = std::time::Instant::now();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .begin_managed_agent(
                "reviewer".into(),
                crate::detect::Agent::Pi,
                now,
                std::time::Duration::ZERO,
                std::time::Duration::from_secs(1),
            );

        let pending = capture_from_state(&state);
        let pending_pane = &pending.workspaces[0].tabs[0].panes[&root.raw()];
        assert_eq!(pending_pane.agent_name, None);
        assert_eq!(pending_pane.managed_agent_kind, None);

        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        assert!(terminal.reconcile_managed_agent_at(now, false));
        let active = capture_from_state(&state);
        let active_pane = &active.workspaces[0].tabs[0].panes[&root.raw()];
        assert_eq!(active_pane.agent_name.as_deref(), Some("reviewer"));
        assert_eq!(active_pane.managed_agent_kind.as_deref(), Some("pi"));
    }

    #[test]
    fn layout_fingerprint_survives_json_round_trip() {
        let mut snapshot = parse_snapshot(include_str!(
            "../../tests/fixtures/session/current-herdr-session.json"
        ))
        .unwrap();
        snapshot.collapsed_space_keys = ["z", "a", "m"].map(String::from).into();
        let expected = layout_fingerprint(&snapshot).unwrap();
        for _ in 0..16 {
            snapshot = parse_snapshot(&serde_json::to_string(&snapshot).unwrap()).unwrap();
            assert_eq!(
                layout_fingerprint(&snapshot).as_deref(),
                Some(expected.as_str())
            );
        }
        snapshot.generation = Some("committed-generation".into());
        assert_eq!(
            layout_fingerprint(&snapshot).as_deref(),
            Some(expected.as_str()),
            "the atomic-write generation is not layout identity"
        );
        snapshot.workspaces.swap(0, 1);
        assert_ne!(
            layout_fingerprint(&snapshot).as_deref(),
            Some(expected.as_str())
        );
    }

    #[test]
    fn round_trip_empty_session() {
        let snap = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![],
            active: None,
            selected: 0,
            sidebar_width: Some(26),
            sidebar_section_split: Some(0.5),
            collapsed_space_keys: std::collections::HashSet::new(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };
        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert!(restored.workspaces.is_empty());
        assert_eq!(restored.active, None);
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
        assert!(!restored.prio_panel_collapsed);
    }

    #[test]
    fn ac3_legacy_v3_snapshot_without_work_context_loads_empty() {
        let raw = r#"{
            "version": 3,
            "workspaces": [{
                "id": "workspace",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": {"Pane": 0},
                    "panes": {"0": {"cwd": "/tmp"}},
                    "zoomed": false
                }]
            }],
            "selected": 0
        }"#;
        let restored = parse_snapshot(raw).expect("legacy v3 snapshot should load");
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].work_context,
            crate::work_context::PaneWorkContext::default()
        );
        assert_eq!(restored.version, 3);
    }

    #[test]
    fn ac3_snapshot_serialization_round_trips_effective_work_context() {
        let mut state = state_with_workspaces(&["context"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                ticket_ids: Some(vec!["MAT-5".into()]),
                work_title: Some("Persist context".into()),
                ..Default::default()
            })
            .unwrap();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .work_context
            .replace_hook_turn(crate::work_context::PaneWorkContext {
                preview_urls: vec!["https://persisted.vercel.app".into()],
                ..Default::default()
            })
            .unwrap();

        let json = serde_json::to_string(&capture_from_state(&state)).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        let context = &restored.workspaces[0].tabs[0].panes[&root.raw()].work_context;
        assert_eq!(context.ticket_ids, vec!["MAT-5"]);
        assert_eq!(context.preview_urls, vec!["https://persisted.vercel.app"]);
        assert_eq!(context.work_title.as_deref(), Some("Persist context"));
    }

    #[test]
    fn legacy_multi_item_snapshot_restores_only_its_previously_primary_assignments() {
        let raw = r#"{
            "version": 3,
            "workspaces": [{
                "id": "workspace",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": {"Pane": 0},
                    "panes": {"0": {
                        "cwd": "/tmp",
                        "work_context": {
                            "ticket_ids": ["SCA-1", "SCA-2"],
                            "pr_urls": [
                                "https://github.com/o/r/pull/1",
                                "https://github.com/o/r/pull/2"
                            ]
                        }
                    }},
                    "zoomed": false
                }]
            }],
            "selected": 0
        }"#;
        let parsed = parse_snapshot(raw).expect("legacy snapshot should parse");
        let saved = &parsed.workspaces[0].tabs[0].panes[&0];
        let mut restored = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/tmp"),
        );

        restored
            .restore_work_context_with_tiers(
                saved.work_context.clone(),
                saved.work_context_tiers.clone(),
            )
            .expect("legacy work context should restore");

        assert_eq!(restored.effective_work_context().ticket_ids, ["SCA-1"]);
        assert_eq!(
            restored.effective_work_context().pr_urls,
            ["https://github.com/o/r/pull/1"]
        );
    }

    #[test]
    fn ac25_v4_snapshot_restore_preserves_git_preview_tier() {
        let state = state_with_workspaces(&["context"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let mut snapshot = capture_from_state(&state);
        let pane = snapshot.workspaces[0].tabs[0]
            .panes
            .get_mut(&root.raw())
            .expect("captured root pane");
        let preview_urls = |prefix: &str, count: usize| {
            (0..count)
                .map(|index| format!("https://{prefix}-{index}.vercel.app"))
                .collect::<Vec<_>>()
        };
        pane.work_context = crate::work_context::PaneWorkContext::default();
        pane.work_context_tiers = Some(crate::work_context::PaneWorkContextTiers {
            manual: crate::work_context::PaneWorkContext {
                preview_urls: preview_urls("manual", crate::work_context::MAX_PREVIEW_URLS),
                ..Default::default()
            },
            hook_turn: crate::work_context::PaneWorkContext {
                preview_urls: preview_urls("hook", 2),
                ..Default::default()
            },
            git_observation: crate::work_context::PaneWorkContext {
                preview_urls: preview_urls("git", crate::work_context::MAX_PREVIEW_URLS),
                ..Default::default()
            },
            restored_fallback: crate::work_context::PaneWorkContext {
                preview_urls: preview_urls("fallback", crate::work_context::MAX_PREVIEW_URLS),
                ..Default::default()
            },
        });

        let parsed = parse_snapshot(&serde_json::to_string(&snapshot).unwrap())
            .expect("v4 snapshot should parse");
        assert_eq!(parsed.version, SNAPSHOT_VERSION);
        let saved = &parsed.workspaces[0].tabs[0].panes[&root.raw()];
        let mut restored = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/tmp"),
        );
        restored
            .restore_work_context_with_tiers(
                saved.work_context.clone(),
                saved.work_context_tiers.clone(),
            )
            .unwrap();

        let tiers = restored.work_context.snapshot_tiers();
        assert!(tiers.manual.preview_urls.is_empty());
        assert_eq!(
            tiers.git_observation.preview_urls,
            preview_urls("git", crate::work_context::MAX_PREVIEW_URLS)
        );
        assert_eq!(
            restored.effective_work_context().preview_urls,
            preview_urls("hook", 2)
                .into_iter()
                .chain(preview_urls(
                    "git",
                    crate::work_context::MAX_PREVIEW_URLS - 2,
                ))
                .collect::<Vec<_>>()
        );
        assert_eq!(
            restored.effective_work_context().preview_urls.len(),
            crate::work_context::MAX_PREVIEW_URLS
        );
    }

    #[test]
    fn ac3_restored_hook_and_git_values_are_replaced_by_new_observations() {
        // Seed hook-only and git-only tiers, capture, serialize, parse, restore
        // into a fresh terminal, then apply NEW observations: they must win.
        let mut state = state_with_workspaces(&["context"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal
            .work_context
            .replace_hook_turn(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["MAT-1".into()],
                preview_urls: vec!["https://old-preview.vercel.app".into()],
                work_title: Some("Old hook title".into()),
                ..Default::default()
            })
            .unwrap();
        terminal
            .work_context
            .replace_git_observation(crate::work_context::PaneWorkContext {
                branch: Some("old-branch".into()),
                ..Default::default()
            })
            .unwrap();

        let json = serde_json::to_string(&capture_from_state(&state)).unwrap();
        let parsed = parse_snapshot(&json).unwrap();
        let saved = &parsed.workspaces[0].tabs[0].panes[&root.raw()];
        let tiers = saved
            .work_context_tiers
            .as_ref()
            .expect("current snapshots preserve source tiers");
        assert_eq!(
            tiers.manual,
            crate::work_context::PaneWorkContext::default()
        );
        assert_eq!(tiers.hook_turn.ticket_ids, vec!["MAT-1".to_string()]);
        assert_eq!(
            tiers.hook_turn.preview_urls,
            vec!["https://old-preview.vercel.app"]
        );
        assert_eq!(
            tiers.hook_turn.work_title.as_deref(),
            Some("Old hook title")
        );
        assert_eq!(tiers.git_observation.branch.as_deref(), Some("old-branch"));
        assert_eq!(
            tiers.restored_fallback,
            crate::work_context::PaneWorkContext::default()
        );

        let mut restored = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/tmp"),
        );
        restored
            .restore_work_context_with_tiers(
                saved.work_context.clone(),
                saved.work_context_tiers.clone(),
            )
            .unwrap();
        assert_eq!(restored.effective_work_context().ticket_ids, vec!["MAT-1"]);
        assert_eq!(
            restored.effective_work_context().preview_urls,
            vec!["https://old-preview.vercel.app"]
        );
        assert_eq!(
            restored.effective_work_context().branch.as_deref(),
            Some("old-branch")
        );

        restored
            .work_context
            .replace_hook_turn(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["MAT-2".into()],
                preview_urls: vec!["https://new-preview.vercel.app".into()],
                work_title: Some("New hook title".into()),
                ..Default::default()
            })
            .unwrap();
        restored
            .work_context
            .replace_git_observation(crate::work_context::PaneWorkContext {
                branch: Some("new-branch".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(restored.effective_work_context().ticket_ids, vec!["MAT-2"]);
        assert_eq!(
            restored.effective_work_context().preview_urls,
            vec!["https://new-preview.vercel.app"]
        );
        assert_eq!(
            restored.effective_work_context().work_title.as_deref(),
            Some("New hook title")
        );
        assert_eq!(
            restored.effective_work_context().branch.as_deref(),
            Some("new-branch")
        );
    }

    #[test]
    fn ac3_restored_manual_values_survive_and_win_over_new_observations() {
        let mut state = state_with_workspaces(&["context"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .apply_manual_work_context_patch(crate::work_context::PaneWorkContextPatch {
                branch: Some("pinned-branch".into()),
                work_title: Some("Pinned title".into()),
                ..Default::default()
            })
            .unwrap();

        let json = serde_json::to_string(&capture_from_state(&state)).unwrap();
        let parsed = parse_snapshot(&json).unwrap();
        let saved = &parsed.workspaces[0].tabs[0].panes[&root.raw()];
        let tiers = saved
            .work_context_tiers
            .as_ref()
            .expect("current snapshots preserve source tiers");
        assert_eq!(tiers.manual.branch.as_deref(), Some("pinned-branch"));
        assert_eq!(tiers.manual.work_title.as_deref(), Some("Pinned title"));
        assert_eq!(
            tiers.hook_turn,
            crate::work_context::PaneWorkContext::default()
        );
        assert_eq!(
            tiers.git_observation,
            crate::work_context::PaneWorkContext::default()
        );
        assert_eq!(
            tiers.restored_fallback,
            crate::work_context::PaneWorkContext::default()
        );

        let mut restored = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/tmp"),
        );
        restored
            .restore_work_context_with_tiers(
                saved.work_context.clone(),
                saved.work_context_tiers.clone(),
            )
            .unwrap();
        restored
            .work_context
            .replace_hook_turn(crate::work_context::PaneWorkContext {
                work_title: Some("Hook title".into()),
                ..Default::default()
            })
            .unwrap();
        restored
            .work_context
            .replace_git_observation(crate::work_context::PaneWorkContext {
                branch: Some("git-branch".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            restored.effective_work_context().work_title.as_deref(),
            Some("Pinned title")
        );
        assert_eq!(
            restored.effective_work_context().branch.as_deref(),
            Some("pinned-branch")
        );
    }

    #[test]
    fn ac3_legacy_flat_work_context_restores_as_lower_precedence_fallback() {
        // A pre-tier snapshot carries only the flat field; it must load intact
        // and never behave like a manual pin after restore.
        let raw = r#"{
            "version": 3,
            "workspaces": [{
                "id": "workspace",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": {"Pane": 0},
                    "panes": {"0": {
                        "cwd": "/tmp",
                        "work_context": {
                            "ticket_ids": ["MAT-1"],
                            "branch": "old-branch",
                            "work_title": "Old title"
                        }
                    }},
                    "zoomed": false
                }]
            }],
            "selected": 0
        }"#;
        let parsed = parse_snapshot(raw).expect("legacy flat snapshot should load");
        let saved = &parsed.workspaces[0].tabs[0].panes[&0];
        assert!(saved.work_context_tiers.is_none());

        let mut restored = crate::terminal::TerminalState::new(
            crate::terminal::TerminalId::alloc(),
            std::path::PathBuf::from("/tmp"),
        );
        restored
            .restore_work_context_with_tiers(
                saved.work_context.clone(),
                saved.work_context_tiers.clone(),
            )
            .unwrap();
        assert_eq!(
            restored.effective_work_context().branch.as_deref(),
            Some("old-branch")
        );

        restored
            .work_context
            .replace_git_observation(crate::work_context::PaneWorkContext {
                branch: Some("new-branch".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(restored.effective_work_context().ticket_ids, ["MAT-1"]);
        assert!(restored.effective_work_context().pr_urls.is_empty());
        assert!(restored.effective_work_context().preview_urls.is_empty());
        assert_eq!(
            restored.effective_work_context().work_title.as_deref(),
            Some("Old title")
        );
        restored
            .work_context
            .replace_hook_turn(crate::work_context::PaneWorkContext {
                ticket_ids: vec!["SCA-9".into()],
                work_title: Some("New title".into()),
                ..Default::default()
            })
            .unwrap();
        assert_eq!(
            restored.effective_work_context().branch.as_deref(),
            Some("new-branch")
        );
        assert_eq!(restored.effective_work_context().ticket_ids, vec!["SCA-9"]);
        assert_eq!(
            restored.effective_work_context().work_title.as_deref(),
            Some("New title")
        );
    }

    #[test]
    fn round_trip_layout_snapshot() {
        let layout = LayoutSnapshot::Split {
            direction: DirectionSnapshot::Horizontal,
            leading: true,
            ratio: 0.6,
            first: Box::new(LayoutSnapshot::Pane(0)),
            second: Box::new(LayoutSnapshot::Split {
                direction: DirectionSnapshot::Vertical,
                leading: true,
                ratio: 0.5,
                first: Box::new(LayoutSnapshot::Pane(1)),
                second: Box::new(LayoutSnapshot::Pane(2)),
            }),
        };
        let json = serde_json::to_string(&layout).unwrap();
        let restored: LayoutSnapshot = serde_json::from_str(&json).unwrap();

        match restored {
            LayoutSnapshot::Split { ratio, .. } => assert!((ratio - 0.6).abs() < 0.01),
            _ => panic!("expected split"),
        }
    }

    #[test]
    fn a_repository_binding_survives_a_session_round_trip() {
        // The binding is the durable half of the feature. If it did not
        // persist, repository organisation would decay across a restart just
        // as it decays across an afternoon.
        let mut ws = crate::workspace::Workspace::test_new("bound");
        ws.repo_binding = Some("matthias-scale/herdr".into());
        let pane_id = ws.tabs[0].root_pane;
        let terminal_id = ws.terminal_id(pane_id).cloned().unwrap();
        let mut terminal = crate::terminal::TerminalState::new(terminal_id.clone(), "/tmp".into());
        terminal.settled_auto_label = Some("#7 Persist settlement".into());
        let terminals = std::collections::HashMap::from([(terminal_id, terminal)]);

        let captured = capture_workspace(
            &ws,
            &terminals,
            &Default::default(),
            Instant::now(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap_or_default()
                .as_secs(),
        );
        let encoded = serde_json::to_string(&captured).expect("encode");
        let decoded: WorkspaceSnapshot = serde_json::from_str(&encoded).expect("decode");

        assert_eq!(
            decoded.repo_binding.as_deref(),
            Some("matthias-scale/herdr")
        );
        assert_eq!(
            decoded.tabs[0].panes[&pane_id.raw()]
                .settled_auto_label
                .as_deref(),
            Some("#7 Persist settlement")
        );
    }

    #[test]
    fn a_session_file_written_before_bindings_existed_still_loads() {
        // The field defaults rather than forcing a snapshot-version migration,
        // so older session files must deserialize unchanged.
        let legacy = serde_json::json!({
            "identity_cwd": "/tmp/herdr",
            "public_pane_numbers": {},
            "next_public_pane_number": 1,
            "public_tab_numbers": [1],
            "next_public_tab_number": 2,
            "tabs": [],
            "active_tab": 0,
        });

        let decoded: WorkspaceSnapshot =
            serde_json::from_value(legacy).expect("legacy snapshot must decode");

        assert_eq!(decoded.repo_binding, None);
    }

    #[test]
    fn round_trip_full_workspace_snapshot() {
        let legacy: PaneSnapshot = serde_json::from_value(serde_json::json!({
            "cwd": "/tmp", "settled_at": null
        }))
        .expect("older pane snapshot still loads");
        assert_eq!(legacy.snoozed_until, None);
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                cwd: PathBuf::from("/home/can/Projects/herdr"),
                is_companion: false,
                last_activity_at: None,
                detection_output_at: None,
                quiet_since_at: None,
                settled_at: Some(1_725_000_000),
                snoozed_until: None,
                settled_work_key: Some("pr:https://github.com/owner/repo/pull/7:merged".into()),
                settled_auto_label: Some("#7 Fix restore".into()),
                work_context: Default::default(),
                work_context_tiers: None,
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                group_membership: Default::default(),
                launch_argv: None,
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                cwd: PathBuf::from("/home/can/Projects/website"),
                is_companion: false,
                last_activity_at: None,
                detection_output_at: None,
                quiet_since_at: None,
                settled_at: None,
                snoozed_until: Some(1_725_000_360),
                settled_work_key: None,
                settled_auto_label: None,
                work_context: Default::default(),
                work_context_tiers: None,
                label: Some("website".into()),
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                group_membership: Default::default(),
                launch_argv: None,
            },
        );

        let snap = SessionSnapshot {
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("wproj".to_string()),
                custom_name: Some("pi-mono".to_string()),
                identity_cwd: PathBuf::from("/home/can/Projects/herdr"),
                worktree_space: None,
                repo_binding: None,
                public_pane_numbers: HashMap::from([(0, 1), (1, 2)]),
                next_public_pane_number: 3,
                public_tab_numbers: vec![1],
                next_public_tab_number: 2,
                tabs: vec![TabSnapshot {
                    custom_name: Some("api".to_string()),
                    name_origin: Default::default(),
                    layout: LayoutSnapshot::Split {
                        direction: DirectionSnapshot::Horizontal,
                        leading: false,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(0)),
                        second: Box::new(LayoutSnapshot::Pane(1)),
                    },
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
            collapsed_space_keys: std::collections::HashSet::new(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
            version: SNAPSHOT_VERSION,
        };

        let json = serde_json::to_string_pretty(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(restored.workspaces[0].id.as_deref(), Some("wproj"));
        assert_eq!(
            restored.workspaces[0].custom_name.as_deref(),
            Some("pi-mono")
        );
        assert_eq!(restored.workspaces[0].tabs.len(), 1);
        assert_eq!(restored.workspaces[0].tabs[0].panes.len(), 2);
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].settled_at,
            Some(1_725_000_000)
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&1].snoozed_until,
            Some(1_725_000_360)
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0]
                .settled_work_key
                .as_deref(),
            Some("pr:https://github.com/owner/repo/pull/7:merged")
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0]
                .settled_auto_label
                .as_deref(),
            Some("#7 Fix restore")
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/home/can/Projects/herdr")
        );
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&1].label.as_deref(),
            Some("website")
        );
        assert_eq!(restored.sidebar_width, Some(26));
        assert_eq!(restored.sidebar_section_split, Some(0.5));
    }

    #[test]
    fn current_session_fixture_parses() {
        let snap = parse_snapshot(session_fixture("current-herdr")).unwrap();

        assert_eq!(snap.version, 3);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(snap.active, Some(0));
        assert_eq!(snap.selected, 0);
        assert_eq!(snap.sidebar_width, None);
        assert_eq!(snap.sidebar_section_split, None);
        assert_eq!(snap.workspaces[0].tabs.len(), 2);
        assert_eq!(
            snap.workspaces[1].identity_cwd,
            PathBuf::from("/home/test/projects/project-b")
        );
    }

    #[test]
    fn current_dev_session_fixture_parses_additive_fields() {
        let snap = parse_snapshot(session_fixture("current-herdr-dev")).unwrap();

        assert_eq!(snap.version, 3);
        assert_eq!(snap.workspaces.len(), 2);
        assert_eq!(snap.sidebar_section_split, Some(0.4));
        assert_eq!(snap.workspaces[0].active_tab, 1);
        assert_eq!(snap.workspaces[1].tabs[0].panes.len(), 2);
    }

    #[test]
    fn old_snapshot_defaults_sidebar_fields() {
        let json = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "workspaces": [],
            "active": null,
            "selected": 0
        })
        .to_string();

        let restored = parse_snapshot(&json).unwrap();

        assert_eq!(restored.sidebar_width, None);
        assert_eq!(restored.sidebar_section_split, None);
        assert!(!restored.prio_panel_collapsed);
    }

    #[test]
    fn old_pane_snapshot_with_embedded_history_is_ignored() {
        let json = serde_json::json!({
            "version": SNAPSHOT_VERSION,
            "workspaces": [{
                "id": "wtest",
                "identity_cwd": "/tmp",
                "tabs": [{
                    "layout": { "Pane": 0 },
                    "panes": {
                        "0": {
                            "cwd": "/tmp",
                            "history": {
                                "ansi": "legacy-secret",
                                "lines": 1
                            }
                        }
                    },
                    "zoomed": false,
                    "focused": 0,
                    "root_pane": 0
                }],
                "active_tab": 0
            }],
            "active": 0,
            "selected": 0
        })
        .to_string();

        let restored = parse_snapshot(&json).unwrap();

        let encoded = serde_json::to_string(&restored).unwrap();
        assert!(!encoded.contains("legacy-secret"));
        assert!(!encoded.contains("\"history\""));
    }

    #[test]
    fn legacy_workspace_snapshot_migrates_to_single_tab() {
        let snap = parse_snapshot(session_fixture("legacy-pre-tabs-v2")).unwrap();
        let ws = &snap.workspaces[0];

        assert_eq!(snap.version, 2);
        assert_eq!(snap.workspaces.len(), 1);
        assert_eq!(ws.custom_name.as_deref(), Some("legacy"));
        assert_eq!(ws.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.active_tab, 0);
        assert_eq!(ws.tabs.len(), 1);
        assert_eq!(ws.tabs[0].focused, Some(1));
        assert_eq!(ws.tabs[0].root_pane, Some(0));
        assert_eq!(ws.tabs[0].panes[&0].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(ws.tabs[0].panes[&1].cwd, PathBuf::from("/tmp/herdr"));
    }

    #[test]
    fn capture_contract_tracks_workspace_order_active_and_selected() {
        let mut state = state_with_workspaces(&["a", "b", "c"]);
        state.active = Some(1);
        state.selected = 2;

        state.move_workspace(1, 0);

        let snapshot = capture_from_state(&state);
        let ids: Vec<_> = state.workspaces.iter().map(|ws| ws.id.clone()).collect();
        let captured_ids: Vec<_> = snapshot
            .workspaces
            .iter()
            .map(|ws| ws.id.clone().unwrap())
            .collect();
        assert_eq!(captured_ids, ids);
        assert_eq!(snapshot.active, state.active);
        assert_eq!(snapshot.selected, state.selected);
    }

    #[test]
    fn capture_contract_tracks_workspace_and_tab_names_and_active_tab() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].set_custom_name("renamed-workspace".into());
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.workspaces[0].switch_tab(second_tab);
        state.workspaces[0].tabs[0].set_custom_name("main".into());

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.custom_name.as_deref(), Some("renamed-workspace"));
        assert_eq!(workspace.active_tab, second_tab);
        assert_eq!(workspace.tabs[0].custom_name.as_deref(), Some("main"));
        assert_eq!(workspace.tabs[1].custom_name.as_deref(), Some("logs"));
    }

    #[test]
    fn tab_snapshot_without_name_origin_defaults_to_structural() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].tabs[0].set_prio(true);
        let snapshot = capture_from_state(&state);
        assert!(snapshot.workspaces[0].tabs[0].prio);
        let mut value = serde_json::to_value(&snapshot.workspaces[0].tabs[0]).unwrap();
        value.as_object_mut().unwrap().remove("name_origin");
        value.as_object_mut().unwrap().remove("prio");

        let restored: TabSnapshot = serde_json::from_value(value).unwrap();

        assert_eq!(
            restored.name_origin,
            crate::workspace::TabNameOrigin::Structural
        );
        assert!(!restored.prio);
    }

    #[test]
    fn capture_contract_tracks_workspace_closure() {
        let mut state = state_with_workspaces(&["one", "two"]);
        state.selected = 1;
        state.active = Some(1);

        state.close_selected_workspace();

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces.len(), 1);
        assert_eq!(snapshot.workspaces[0].custom_name.as_deref(), Some("one"));
        assert_eq!(snapshot.active, Some(0));
        assert_eq!(snapshot.selected, 0);
    }

    #[test]
    fn capture_contract_preserves_fork_server_chrome_state() {
        let mut state = state_with_workspaces(&["one"]);
        state.sidebar_width = 31;
        state.sidebar_section_split = 0.4;
        state.collapsed_space_keys.insert("repo:one".into());
        state.prio_panel_collapsed = true;

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.sidebar_width, Some(31));
        assert_eq!(snapshot.sidebar_section_split, Some(0.4));
        assert_eq!(snapshot.collapsed_space_keys, state.collapsed_space_keys);
        assert!(snapshot.prio_panel_collapsed);
    }

    #[test]
    fn capture_contract_tracks_worktree_space_membership() {
        let mut state = state_with_workspaces(&["main"]);
        state.workspaces[0].worktree_space = Some(crate::workspace::WorktreeSpaceMembership {
            key: "repo-key".into(),
            label: "herdr".into(),
            repo_root: PathBuf::from("/repo/herdr"),
            checkout_path: PathBuf::from("/repo/herdr/worktree-a"),
            is_linked_worktree: true,
        });

        let snapshot = capture_from_state(&state);

        assert_eq!(
            snapshot.workspaces[0].worktree_space,
            state.workspaces[0].worktree_space
        );
    }

    #[test]
    fn capture_contract_tracks_layout_focus_zoom_and_root_pane() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].tabs[0].layout.focus_pane(second);
        state.toggle_zoom();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert!(matches!(tab.layout, LayoutSnapshot::Split { .. }));
        assert_eq!(tab.focused, Some(second.raw()));
        assert_eq!(tab.root_pane, Some(root.raw()));
        assert!(tab.zoomed);
        assert_eq!(tab.panes.len(), 2);
    }

    #[test]
    fn capture_contract_tracks_focus_navigation() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        crate::ui::compute_view_with_runtime_registry(
            &mut state,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 106, 20),
        );

        state.navigate_pane(NavDirection::Right);

        let snapshot = capture_from_state(&state);
        assert_eq!(snapshot.workspaces[0].tabs[0].focused, Some(second.raw()));
        assert_ne!(snapshot.workspaces[0].tabs[0].focused, Some(root.raw()));
    }

    #[test]
    fn capture_contract_tracks_resize_ratio_changes() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.workspaces[0].test_split(Direction::Horizontal);
        state.workspaces[0].layout.focus_pane(root);
        crate::ui::compute_view_with_runtime_registry(
            &mut state,
            &crate::terminal::TerminalRuntimeRegistry::new(),
            Rect::new(0, 0, 106, 20),
        );
        let before = capture_from_state(&state);

        state.resize_pane(NavDirection::Right);

        let after = capture_from_state(&state);
        let before_ratio = root_split_ratio(&before.workspaces[0].tabs[0]).unwrap();
        let after_ratio = root_split_ratio(&after.workspaces[0].tabs[0]).unwrap();
        assert_ne!(before_ratio, after_ratio);
    }

    #[test]
    fn capture_contract_tracks_tab_closure() {
        let mut state = state_with_workspaces(&["one"]);
        let second_tab = state.workspaces[0].test_add_tab(Some("logs"));
        state.switch_tab(second_tab);

        state.close_tab();

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(workspace.tabs.len(), 1);
        assert_eq!(workspace.active_tab, 0);
        assert!(workspace.tabs[0].custom_name.is_none());
    }

    #[test]
    fn capture_contract_tracks_pane_closure() {
        let mut state = state_with_workspaces(&["one"]);
        state.workspaces[0].test_split(Direction::Horizontal);

        state.close_pane();

        let snapshot = capture_from_state(&state);
        let tab = &snapshot.workspaces[0].tabs[0];
        assert_eq!(tab.panes.len(), 1);
        assert!(matches!(tab.layout, LayoutSnapshot::Pane(_)));
        assert!(!tab.zoomed);
    }

    #[test]
    fn capture_contract_tracks_public_id_counters() {
        let mut state = state_with_workspaces(&["one"]);
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let third = state.workspaces[0].test_split(Direction::Vertical);
        let second_tab = state.workspaces[0].test_add_tab(None);

        state.workspaces[0].close_pane(second);

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        assert_eq!(
            workspace.public_pane_numbers,
            HashMap::from([
                (state.workspaces[0].tabs[0].root_pane.raw(), 1),
                (third.raw(), 3),
                (state.workspaces[0].tabs[second_tab].root_pane.raw(), 4),
            ])
        );
        assert_eq!(workspace.next_public_pane_number, 5);
        assert_eq!(workspace.public_tab_numbers, vec![1, 2]);
        assert_eq!(workspace.next_public_tab_number, 3);
    }

    #[cfg(unix)]
    #[tokio::test]
    async fn capture_prefers_live_shell_cwd_and_keeps_it_after_exit() {
        let old = std::env::current_dir().unwrap();
        let new = std::env::temp_dir().join(format!(
            "herdr-persist-cwd-{}-{}",
            std::process::id(),
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&new).unwrap();
        let new = std::fs::canonicalize(new).unwrap();
        let mut state = AppState::test_new();
        state.workspaces = vec![Workspace::test_new("cwd-source")];
        state.workspaces[0].identity_cwd = old.clone();
        state.active = Some(0);
        state.ensure_test_terminals();
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].terminal_id(pane_id).unwrap().clone();
        let (events, _rx) = tokio::sync::mpsc::channel(32);
        let runtime = crate::terminal::TerminalRuntime::spawn(
            pane_id,
            24,
            80,
            old.clone(),
            0,
            Default::default(),
            None,
            crate::pane::PaneShellConfig::new("/bin/sh", crate::config::ShellModeConfig::NonLogin),
            &crate::pane::PaneLaunchEnv::default(),
            events,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::new()),
        )
        .unwrap();
        let pid = runtime.child_pid().unwrap();
        runtime
            .try_send_bytes(bytes::Bytes::from(format!(
                "cd '{}'; printf '\\033]7;file://{}\\007'; exec sleep 30\n",
                new.display(),
                old.display()
            )))
            .unwrap();
        let deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while (crate::platform::process_cwd(pid).as_ref() != Some(&new)
            || runtime.cwd().as_ref() != Some(&old))
            && std::time::Instant::now() < deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert_eq!(crate::platform::process_cwd(pid), Some(new.clone()));
        assert_eq!(
            runtime.cwd(),
            Some(old.clone()),
            "existing reported-cwd accessor is unchanged"
        );
        let mut runtimes = TerminalRuntimeRegistry::new();
        runtimes.insert(terminal_id, runtime);
        let before = capture_from_state_with_runtimes(&state, &runtimes);
        assert_eq!(
            before.workspaces[0].tabs[0]
                .panes
                .values()
                .next()
                .unwrap()
                .cwd,
            new
        );
        assert_eq!(before.workspaces[0].identity_cwd, new);
        assert_eq!(runtimes.values().next().unwrap().cwd(), Some(old.clone()));
        crate::platform::signal_processes(&[pid], crate::platform::Signal::Kill);
        let exit_deadline = std::time::Instant::now() + std::time::Duration::from_secs(5);
        while crate::platform::process_cwd(pid).is_some()
            && std::time::Instant::now() < exit_deadline
        {
            tokio::time::sleep(std::time::Duration::from_millis(10)).await;
        }
        assert!(crate::platform::process_cwd(pid).is_none());
        let after = capture_from_state_with_runtimes(&state, &runtimes);
        assert_eq!(
            after.workspaces[0].tabs[0]
                .panes
                .values()
                .next()
                .unwrap()
                .cwd,
            new
        );
        assert_eq!(after.workspaces[0].identity_cwd, new);
        assert_eq!(runtimes.values().next().unwrap().cwd(), Some(old));
        for (_, runtime) in runtimes.drain() {
            runtime.shutdown();
        }
        std::fs::remove_dir(new).unwrap();
    }

    #[test]
    fn capture_contract_tracks_workspace_identity_and_pane_cwds() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.workspaces[0].identity_cwd = PathBuf::from("/tmp/pion");
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        state.ensure_test_terminals();
        let root_terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&root_terminal_id).unwrap().cwd = PathBuf::from("/tmp/pion");
        let second_terminal_id = state.workspaces[0].tabs[0].panes[&second]
            .attached_terminal_id
            .clone();
        state.terminals.get_mut(&second_terminal_id).unwrap().cwd = PathBuf::from("/tmp/herdr");

        let snapshot = capture_from_state(&state);
        let workspace = &snapshot.workspaces[0];
        let tab = &workspace.tabs[0];
        assert_eq!(workspace.identity_cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&root.raw()].cwd, PathBuf::from("/tmp/pion"));
        assert_eq!(tab.panes[&second.raw()].cwd, PathBuf::from("/tmp/herdr"));
    }

    #[tokio::test]
    async fn capture_contract_tracks_pane_history_from_runtime() {
        let state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"alpha\r\nbeta\r\ngamma\r\n",
            ),
        );

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("alpha"));
        assert!(!encoded.contains("\"history\""));

        let history_snapshot = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        let history = &history_snapshot.workspaces[0].tabs[0].panes[&root.raw()];

        // ac3: history carries the stable identity used to reject raw-ID reuse.
        let expected_pane_id = crate::workspace::public_pane_id_for_number(
            &state.workspaces[0].id,
            state.workspaces[0].public_pane_number(root).unwrap(),
        );
        assert_eq!(history.pane_id.as_deref(), Some(expected_pane_id.as_str()));
        assert!(history.ansi.contains("alpha"));
        assert!(history.ansi.contains("gamma"));
        assert!(history.lines >= 3);
    }

    #[tokio::test]
    async fn capture_contract_tracks_history_for_each_pane() {
        let mut state = state_with_workspaces(&["one"]);
        let first = state.workspaces[0].tabs[0].root_pane;
        let second = state.workspaces[0].test_split(Direction::Horizontal);
        let first_terminal_id = state.workspaces[0].tabs[0].panes[&first]
            .attached_terminal_id
            .clone();
        let second_terminal_id = state.workspaces[0].tabs[0].panes[&second]
            .attached_terminal_id
            .clone();
        let mut terminal_runtimes = TerminalRuntimeRegistry::new();
        terminal_runtimes.insert(
            first_terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"first-pane-history\r\n",
            ),
        );
        terminal_runtimes.insert(
            second_terminal_id,
            crate::terminal::TerminalRuntime::test_with_scrollback_bytes(
                20,
                3,
                4096,
                b"second-pane-history\r\n",
            ),
        );

        let snapshot = capture_from_state_with_runtimes(&state, &terminal_runtimes);
        let encoded = serde_json::to_string(&snapshot).unwrap();
        assert!(!encoded.contains("first-pane-history"));
        assert!(!encoded.contains("second-pane-history"));

        let history_snapshot = capture_history_from_state_with_runtimes(&state, &terminal_runtimes);
        let tab = &history_snapshot.workspaces[0].tabs[0];
        let first_history = &tab.panes[&first.raw()];
        let second_history = &tab.panes[&second.raw()];

        assert!(first_history.ansi.contains("first-pane-history"));
        assert!(second_history.ansi.contains("second-pane-history"));
    }

    #[test]
    fn capture_contract_tracks_hook_authority_agent_session() {
        let mut state = state_with_workspaces(&["one"]);
        let session_path = test_session_path("pi-session.jsonl");
        let root = state.workspaces[0].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        });
        terminal.set_hook_authority_with_session_ref(
            "herdr:pi".into(),
            "pi".into(),
            crate::detect::AgentState::Working,
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
        );

        let snapshot = capture_from_state(&state);
        let agent_session = snapshot.workspaces[0].tabs[0].panes[&root.raw()]
            .agent_session
            .as_ref()
            .expect("agent session should be captured");

        assert_eq!(agent_session.source, "herdr:pi");
        assert_eq!(agent_session.agent, "pi");
        assert_eq!(
            agent_session.kind,
            crate::agent_resume::AgentSessionRefKind::Path
        );
        assert_eq!(agent_session.value, session_path);
    }

    /// Build a pane whose agent has reported `state` through the closing-block
    /// hook, and return what capture wrote for it.
    fn captured_agent_session_for_reported_state(
        state_reported: crate::detect::AgentState,
        retire: bool,
    ) -> Option<PaneAgentSessionSnapshot> {
        let mut state = state_with_workspaces(&["one"]);
        let session_path = test_session_path("pi-session.jsonl");
        let root = state.workspaces[0].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        let terminal = state.terminals.get_mut(&terminal_id).unwrap();
        terminal.set_detected_state(
            Some(crate::detect::Agent::Pi),
            crate::detect::AgentState::Idle,
        );
        // The hook report is only routed onto a pane whose session identity is
        // already anchored, which is what the agent's own integration does.
        terminal.set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
            source: "herdr:pi".into(),
            agent: "pi".into(),
            session_ref: crate::agent_resume::AgentSessionRef::path(session_path.clone()).unwrap(),
        });
        terminal.set_hook_authority_report_at(
            "herdr:pi".into(),
            "pi".into(),
            state_reported,
            Some("needs a decision".into()),
            Some("review".into()),
            Some(90),
            None,
            crate::agent_resume::AgentSessionRef::path(session_path.clone()),
            Some(20),
            std::time::Instant::now(),
        );
        if retire {
            terminal.retire_blocked_full_lifecycle_hook_authority_at(std::time::Instant::now());
        }

        let snapshot = capture_from_state(&state);
        snapshot.workspaces[0].tabs[0].panes[&root.raw()]
            .agent_session
            .clone()
    }

    #[test]
    fn capture_carries_a_live_blocked_report_with_its_message() {
        let session =
            captured_agent_session_for_reported_state(crate::detect::AgentState::Blocked, false)
                .expect("agent session should be captured");
        let blocked = session
            .blocked
            .as_ref()
            .expect("a live blocked report must survive capture");

        assert_eq!(blocked.message.as_deref(), Some("needs a decision"));
    }

    #[test]
    fn capture_leaves_a_working_report_unblocked() {
        // Working is re-derived from the screen on restore, so persisting it
        // would only let a stale report outrank live detection.
        let session =
            captured_agent_session_for_reported_state(crate::detect::AgentState::Working, false)
                .expect("agent session should be captured");

        assert_eq!(session.blocked, None);
    }

    #[test]
    fn capture_drops_a_retired_blocked_report() {
        // A retired report has stopped speaking for the pane; a restart must not
        // be the thing that brings it back.
        let session =
            captured_agent_session_for_reported_state(crate::detect::AgentState::Blocked, true)
                .expect("agent session should be captured");

        assert_eq!(session.blocked, None);
    }

    #[test]
    fn agent_session_without_a_blocked_field_loads_as_not_blocked() {
        // Sessions written before this field existed must still parse.
        let session: PaneAgentSessionSnapshot =
            serde_json::from_str(r#"{"source":"herdr:pi","agent":"pi","kind":"id","value":"s1"}"#)
                .expect("legacy agent session should parse");

        assert_eq!(session.blocked, None);
    }

    #[test]
    fn capture_contract_preserves_restored_agent_session() {
        let mut state = state_with_workspaces(&["one"]);
        let root = state.workspaces[0].tabs[0].root_pane;
        state.ensure_test_terminals();
        let terminal_id = state.workspaces[0].tabs[0].panes[&root]
            .attached_terminal_id
            .clone();
        state
            .terminals
            .get_mut(&terminal_id)
            .unwrap()
            .set_persisted_agent_session(crate::agent_resume::PersistedAgentSession {
                source: "herdr:opencode".into(),
                agent: "opencode".into(),
                session_ref: crate::agent_resume::AgentSessionRef::id("opencode-session").unwrap(),
            });

        let snapshot = capture_from_state(&state);
        let agent_session = snapshot.workspaces[0].tabs[0].panes[&root.raw()]
            .agent_session
            .as_ref()
            .expect("persisted agent session should be captured");

        assert_eq!(agent_session.source, "herdr:opencode");
        assert_eq!(agent_session.agent, "opencode");
        assert_eq!(
            agent_session.kind,
            crate::agent_resume::AgentSessionRefKind::Id
        );
        assert_eq!(agent_session.value, "opencode-session");
    }

    #[test]
    fn capture_persists_the_observed_quiet_clock() {
        let mut state = state_with_workspaces(&["quiet"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let quiet_since = Instant::now() - Duration::from_secs(90);
        let pane = state.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        pane.activity.observe_quiet(true, quiet_since);

        let captured = capture_from_state(&state);
        let saved = &captured.workspaces[0].tabs[0].panes[&pane_id.raw()];

        assert!(saved.quiet_since_at.is_some());
    }

    #[test]
    fn capture_keeps_detection_output_older_than_later_input_activity() {
        let mut state = state_with_workspaces(&["output-clock"]);
        let pane_id = state.workspaces[0].tabs[0].root_pane;
        let now = Instant::now();
        let pane = state.workspaces[0].tabs[0].panes.get_mut(&pane_id).unwrap();
        assert!(!pane.activity.observe_detection_snapshot(
            1,
            Some(crate::detect::Agent::Claude),
            "waiting",
            now - Duration::from_secs(180),
        ));
        assert!(pane.activity.observe_detection_snapshot(
            2,
            Some(crate::detect::Agent::Claude),
            "subagent completed",
            now - Duration::from_secs(120),
        ));
        pane.activity.note(now - Duration::from_secs(60));

        let captured = capture_from_state(&state);
        let json = serde_json::to_string(&captured).unwrap();
        let restored: SessionSnapshot = serde_json::from_str(&json).unwrap();
        let saved = &restored.workspaces[0].tabs[0].panes[&pane_id.raw()];

        assert!(saved.last_activity_at > saved.detection_output_at);
    }

    #[test]
    fn pane_snapshot_without_a_quiet_clock_stays_compatible() {
        let legacy = serde_json::json!({
            "cwd": "/tmp",
            "work_context": {}
        });

        let pane: PaneSnapshot = serde_json::from_value(legacy).unwrap();

        assert_eq!(pane.quiet_since_at, None);
        assert_eq!(pane.detection_output_at, None);
    }

    #[test]
    fn old_unversioned_snapshot_loads_as_version_0() {
        let json = r#"{"workspaces":[],"active":null,"selected":0}"#;
        let snap = parse_snapshot(json).unwrap();
        assert_eq!(snap.version, 0);
    }

    #[test]
    fn future_version_is_rejected() {
        let json = r#"{"version":999,"workspaces":[],"active":null,"selected":0}"#;
        assert!(parse_snapshot(json).is_err());
    }

    #[test]
    fn active_tab_default_is_zero() {
        let json = r#"{"custom_name":"test","identity_cwd":"/tmp","tabs":[]}"#;
        let ws: WorkspaceSnapshot = serde_json::from_str(json).unwrap();
        assert_eq!(ws.active_tab, 0);
    }

    #[test]
    fn snapshot_parsing_preserves_missing_cwd() {
        let mut panes = HashMap::new();
        panes.insert(
            0,
            PaneSnapshot {
                cwd: PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test"),
                is_companion: false,
                last_activity_at: None,
                detection_output_at: None,
                quiet_since_at: None,
                settled_at: None,
                snoozed_until: None,
                settled_work_key: None,
                settled_auto_label: None,
                work_context: Default::default(),
                work_context_tiers: None,
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                group_membership: Default::default(),
                launch_argv: None,
            },
        );
        panes.insert(
            1,
            PaneSnapshot {
                cwd: std::env::var("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_else(|_| PathBuf::from("/tmp")),
                is_companion: false,
                last_activity_at: None,
                detection_output_at: None,
                quiet_since_at: None,
                settled_at: None,
                snoozed_until: None,
                settled_work_key: None,
                settled_auto_label: None,
                work_context: Default::default(),
                work_context_tiers: None,
                label: None,
                agent_name: None,
                managed_agent_kind: None,
                agent_session: None,
                group_membership: Default::default(),
                launch_argv: None,
            },
        );

        let snap = SessionSnapshot {
            version: SNAPSHOT_VERSION,
            generation: None,
            workspaces: vec![WorkspaceSnapshot {
                is_fleet: false,
                repo_binding_cleared: false,
                id: Some("test-ws".to_string()),
                custom_name: Some("fallback test".to_string()),
                identity_cwd: PathBuf::from("/tmp"),
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
                        direction: DirectionSnapshot::Horizontal,
                        leading: false,
                        ratio: 0.5,
                        first: Box::new(LayoutSnapshot::Pane(0)),
                        second: Box::new(LayoutSnapshot::Pane(1)),
                    },
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
            collapsed_space_keys: std::collections::HashSet::new(),
            prio_panel_collapsed: false,
            window_cycle_mode: None,
            skip_collapsed_cycle: None,
        };

        let json = serde_json::to_string(&snap).unwrap();
        let restored = parse_snapshot(&json).unwrap();
        assert_eq!(restored.workspaces.len(), 1);
        assert_eq!(
            restored.workspaces[0].tabs[0].panes[&0].cwd,
            PathBuf::from("/tmp/this-directory-does-not-exist-for-herdr-test")
        );
    }
}
