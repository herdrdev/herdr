//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::{AppState, DockInputOwner, InputOwner};
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{CursorState, FrameData, RenderEncoding, ServerMessage, TerminalFrame};
use crate::terminal::TerminalRuntimeRegistry;

/// Per-client render baseline for the negotiated render encoding.
pub(crate) enum ClientRenderState {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic { last_frame: Option<FrameData> },
    /// Terminal-ANSI clients keep a terminal diff encoder and sequence number.
    TerminalAnsi {
        blit_encoder: BlitEncoder,
        seq: u64,
        repaint_pending: bool,
    },
}

impl ClientRenderState {
    pub(crate) fn new(render_encoding: RenderEncoding) -> Self {
        match render_encoding {
            RenderEncoding::SemanticFrame => Self::Semantic { last_frame: None },
            RenderEncoding::TerminalAnsi => Self::TerminalAnsi {
                blit_encoder: BlitEncoder::new(),
                seq: 0,
                repaint_pending: false,
            },
        }
    }

    pub(crate) fn reset_baseline(&mut self) {
        match self {
            Self::Semantic { last_frame } => *last_frame = None,
            Self::TerminalAnsi {
                blit_encoder,
                repaint_pending,
                ..
            } => {
                *blit_encoder = BlitEncoder::new();
                *repaint_pending = false;
            }
        }
    }

    pub(crate) fn request_repaint(&mut self) {
        match self {
            Self::Semantic { last_frame } => *last_frame = None,
            Self::TerminalAnsi {
                repaint_pending, ..
            } => *repaint_pending = true,
        }
    }

    pub(crate) fn reset_semantic_input_baseline(&mut self) {
        if let Self::Semantic { last_frame } = self {
            *last_frame = None;
        }
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        match self {
            Self::Semantic { last_frame } => {
                if last_frame.as_ref() == Some(&frame) {
                    crate::render_prof::event("prepare_frame.semantic.skip_current");
                    return None;
                }
                crate::render_prof::event("prepare_frame.semantic.changed");
                Some(PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                })
            }
            Self::TerminalAnsi {
                blit_encoder,
                seq,
                repaint_pending,
            } => {
                if !*repaint_pending && blit_encoder.is_current(&frame) {
                    crate::render_prof::event("prepare_frame.ansi.skip_current");
                    return None;
                }
                let mut encoded = blit_encoder.encode(&frame, *repaint_pending);
                crate::render_prof::event("prepare_frame.ansi.changed");
                crate::render_prof::counter("prepare_frame.ansi.bytes", encoded.bytes.len() as u64);
                if encoded.full {
                    crate::render_prof::event("prepare_frame.ansi.full");
                } else {
                    crate::render_prof::event("prepare_frame.ansi.partial");
                }
                insert_graphics_before_sync_end(&mut encoded.bytes, &frame.graphics);
                crate::render_prof::counter(
                    "prepare_frame.graphics.bytes",
                    frame.graphics.len() as u64,
                );
                Some(PreparedRender::TerminalAnsi {
                    message: ServerMessage::Terminal(TerminalFrame {
                        seq: *seq + 1,
                        width: frame.width,
                        height: frame.height,
                        full: encoded.full,
                        bytes: encoded.bytes.clone(),
                    }),
                    frame,
                    encoded: Some(encoded),
                })
            }
        }
    }

    pub(crate) fn last_frame(&self) -> Option<&FrameData> {
        match self {
            Self::Semantic { last_frame } => last_frame.as_ref(),
            Self::TerminalAnsi { blit_encoder, .. } => blit_encoder.last_frame(),
        }
    }

    pub(crate) fn take_semantic_frame(&mut self) -> Option<FrameData> {
        match self {
            Self::Semantic { last_frame } => last_frame.take(),
            Self::TerminalAnsi { .. } => None,
        }
    }

    pub(crate) fn restore_semantic_frame(&mut self, frame: FrameData) {
        if let Self::Semantic { last_frame } = self {
            *last_frame = Some(frame);
        }
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        match (self, prepared) {
            (
                Self::Semantic { last_frame },
                PreparedRender::Semantic {
                    message: ServerMessage::Frame(frame),
                },
            ) => *last_frame = Some(frame),
            (
                Self::TerminalAnsi {
                    blit_encoder,
                    seq,
                    repaint_pending,
                },
                PreparedRender::TerminalAnsi {
                    frame,
                    encoded: Some(encoded),
                    ..
                },
            ) => {
                blit_encoder.commit(frame, encoded);
                *seq += 1;
                *repaint_pending = false;
            }
            _ => {}
        }
    }

    #[cfg(test)]
    pub(crate) fn terminal_seq(&self) -> Option<u64> {
        match self {
            Self::Semantic { .. } => None,
            Self::TerminalAnsi { seq, .. } => Some(*seq),
        }
    }
}

fn insert_graphics_before_sync_end(encoded: &mut Vec<u8>, graphics: &[u8]) {
    if graphics.is_empty() {
        return;
    }

    if let Some(sync_end) = crate::protocol::render_ansi::final_sync_output_end(encoded) {
        encoded.splice(sync_end..sync_end, graphics.iter().copied());
    } else {
        encoded.extend_from_slice(graphics);
    }
}

fn focused_agent_pane_id(app_state: &AppState) -> Option<crate::layout::PaneId> {
    let ws_idx = app_state.active?;
    let workspace = app_state.workspaces.get(ws_idx)?;
    let pane_id = workspace.focused_pane_id()?;
    let terminal_id = workspace.terminal_id(pane_id)?;
    let terminal = app_state.terminals.get(terminal_id)?;
    terminal.is_agent_terminal().then_some(pane_id)
}

fn focused_editor_terminal_id(app_state: &AppState) -> Option<crate::terminal::TerminalId> {
    if app_state.input_owner() != InputOwner::Dock(DockInputOwner::Editor) {
        return None;
    }
    focused_editor_terminal_id_when_owned(app_state)
}

fn focused_editor_terminal_id_when_owned(
    app_state: &AppState,
) -> Option<crate::terminal::TerminalId> {
    if !app_state.dock_editor_focused
        || app_state.dock_collapsed
        || app_state.dock_tab != Some(crate::app::DockSurface::Editor)
    {
        return None;
    }
    let agent_pane_id = focused_agent_pane_id(app_state)?;
    app_state
        .dock_editor_sessions
        .get(&agent_pane_id)
        .map(|session| session.terminal_id.clone())
}

pub(crate) fn dock_editor_is_focused(app_state: &AppState) -> bool {
    focused_editor_terminal_id(app_state).is_some()
}

fn dock_editor_cursor_state_when_owned(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    let terminal_id = focused_editor_terminal_id_when_owned(app_state)?;
    dock_editor_cursor_state_for_terminal(app_state, terminal_runtimes, &terminal_id)
}

fn dock_editor_cursor_state_for_terminal(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    terminal_id: &crate::terminal::TerminalId,
) -> Option<CursorState> {
    let runtime = terminal_runtimes.get(terminal_id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let cursor = runtime.cursor_state(app_state.view.dock_body_rect, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
        shape: cursor.shape,
    })
}
/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
    },
    TerminalAnsi {
        message: ServerMessage,
        frame: FrameData,
        encoded: Option<EncodedBlit>,
    },
}

impl PreparedRender {
    pub(crate) fn message(&self) -> &ServerMessage {
        match self {
            Self::Semantic { message } | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn into_frame(self) -> Option<FrameData> {
        match self {
            Self::Semantic {
                message: ServerMessage::Frame(frame),
            } => Some(frame),
            Self::TerminalAnsi { frame, .. } => Some(frame),
            _ => None,
        }
    }
}

struct CursorTrackingBackend {
    inner: TestBackend,
    rendered_cursor: Option<Position>,
}

impl CursorTrackingBackend {
    fn new(width: u16, height: u16) -> Self {
        Self {
            inner: TestBackend::new(width, height),
            rendered_cursor: None,
        }
    }

    fn buffer(&self) -> &ratatui::buffer::Buffer {
        self.inner.buffer()
    }

    fn rendered_cursor(&self) -> Option<CursorState> {
        self.rendered_cursor.map(|pos| CursorState {
            x: pos.x,
            y: pos.y,
            visible: true,
            shape: 0,
        })
    }

    fn clear_rendered_cursor(&mut self) {
        self.rendered_cursor = None;
    }
}

/// A virtual Ratatui terminal retained by one connected client.
///
/// Its size changes only when that client resizes. Keeping the terminal alive
/// reuses Ratatui's front and back buffers across ordinary full renders.
pub(crate) struct VirtualTerminal {
    size: (u16, u16),
    terminal: ratatui::Terminal<CursorTrackingBackend>,
}

impl VirtualTerminal {
    fn new(width: u16, height: u16) -> Self {
        let backend = CursorTrackingBackend::new(width, height);
        let terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");
        Self {
            size: (width, height),
            terminal,
        }
    }
}

impl Backend for CursorTrackingBackend {
    type Error = std::convert::Infallible;

    fn draw<'a, I>(&mut self, content: I) -> Result<(), Self::Error>
    where
        I: Iterator<Item = (u16, u16, &'a ratatui::buffer::Cell)>,
    {
        self.inner.draw(content)
    }

    fn append_lines(&mut self, n: u16) -> Result<(), Self::Error> {
        self.inner.append_lines(n)
    }

    fn hide_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.hide_cursor()?;
        self.rendered_cursor = None;
        Ok(())
    }

    fn show_cursor(&mut self) -> Result<(), Self::Error> {
        self.inner.show_cursor()
    }

    fn get_cursor_position(&mut self) -> Result<Position, Self::Error> {
        self.inner.get_cursor_position()
    }

    fn set_cursor_position<P: Into<Position>>(&mut self, position: P) -> Result<(), Self::Error> {
        let position = position.into();
        self.inner.set_cursor_position(position)?;
        self.rendered_cursor = Some(position);
        Ok(())
    }

    fn clear(&mut self) -> Result<(), Self::Error> {
        self.inner.clear()
    }

    fn clear_region(&mut self, clear_type: ClearType) -> Result<(), Self::Error> {
        self.inner.clear_region(clear_type)
    }

    fn size(&self) -> Result<Size, Self::Error> {
        self.inner.size()
    }

    fn window_size(&mut self) -> Result<WindowSize, Self::Error> {
        self.inner.window_size()
    }

    fn flush(&mut self) -> Result<(), Self::Error> {
        self.inner.flush()
    }
}

/// Renders the AppState to an in-memory ratatui Buffer.
///
/// This produces the same output as the monolithic binary's terminal draw,
/// but writes to a `Buffer` instead of stdout. Cursor visibility is captured
/// from explicit frame cursor intent rather than incidental backend state.
#[cfg_attr(not(test), allow(dead_code))]
pub(crate) fn render_virtual(
    app_state: &mut AppState,
    area: Rect,
    resize_panes: bool,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let terminal_runtimes = TerminalRuntimeRegistry::new();
    render_virtual_with_runtime_registry(
        app_state,
        &terminal_runtimes,
        area,
        resize_panes,
        crate::kitty_graphics::HostCellSize::default(),
    )
}

pub(crate) fn render_virtual_with_runtime_registry(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let mut terminal = VirtualTerminal::new(area.width, area.height);
    render_virtual_with_runtime_registry_using_terminal(
        app_state,
        terminal_runtimes,
        area,
        resize_panes,
        cell_size,
        &mut terminal,
    )
}

pub(crate) fn render_virtual_with_runtime_registry_reusing(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    terminal: &mut Option<VirtualTerminal>,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    if terminal
        .as_ref()
        .is_none_or(|terminal| terminal.size != (area.width, area.height))
    {
        *terminal = Some(VirtualTerminal::new(area.width, area.height));
    }
    let terminal = terminal
        .as_mut()
        .expect("virtual terminal was initialized for this client size");
    render_virtual_with_runtime_registry_using_terminal(
        app_state,
        terminal_runtimes,
        area,
        resize_panes,
        cell_size,
        terminal,
    )
}

fn render_virtual_with_runtime_registry_using_terminal(
    app_state: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
    terminal: &mut VirtualTerminal,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let owner = app_state.input_owner();
    let popup_visible = owner == InputOwner::Popup;
    let dock_editor_focused = owner == InputOwner::Dock(DockInputOwner::Editor);
    let pane_owns_input = owner == InputOwner::Pane;
    let pre_compute_suppresses_focused_terminal_cursor = pane_owns_input
        && focused_terminal_suppresses_host_cursor_when_owned(app_state, terminal_runtimes);
    if resize_panes {
        crate::ui::compute_view_with_cell_size(app_state, terminal_runtimes, area, cell_size);
    } else {
        crate::ui::compute_view_without_resizing_panes(app_state, terminal_runtimes, area);
    }
    let suppress_focused_terminal_cursor = pre_compute_suppresses_focused_terminal_cursor
        || (pane_owns_input
            && focused_terminal_suppresses_host_cursor_when_owned(app_state, terminal_runtimes));

    terminal.terminal.backend_mut().clear_rendered_cursor();
    terminal
        .terminal
        .draw(|frame| {
            crate::ui::render_with_runtime_registry_for_owner(
                app_state,
                terminal_runtimes,
                frame,
                owner,
            );
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.terminal.backend().buffer().clone();
    let cursor = if popup_visible {
        popup_terminal_cursor(app_state, terminal_runtimes)
    } else if dock_editor_focused {
        dock_editor_cursor_state_when_owned(app_state, terminal_runtimes)
    } else if !pane_owns_input {
        terminal.terminal.backend().rendered_cursor()
    } else if suppress_focused_terminal_cursor {
        None
    } else {
        crate::ui::tab_surface_cursor(app_state, terminal_runtimes, app_state.view.tab_surface())
            .or_else(|| {
                (!focused_terminal_owns_host_cursor_when_owned(app_state, terminal_runtimes))
                    .then(|| terminal.terminal.backend().rendered_cursor())
                    .flatten()
            })
    };

    (buffer, cursor)
}

pub(crate) fn render_hyperspace_animation_buffer_reusing(
    app_state: &AppState,
    area: Rect,
    hovered: bool,
    terminal: &mut Option<VirtualTerminal>,
) -> ratatui::buffer::Buffer {
    if terminal
        .as_ref()
        .is_none_or(|terminal| terminal.size != (area.width, area.height))
    {
        *terminal = Some(VirtualTerminal::new(area.width, area.height));
    }
    let terminal = terminal
        .as_mut()
        .expect("animation terminal was initialized for this client size");
    terminal.terminal.backend_mut().clear_rendered_cursor();
    terminal
        .terminal
        .draw(|frame| {
            crate::ui::hyperspace::render_animation_with_hover(
                app_state,
                frame,
                frame.area(),
                hovered,
            )
        })
        .expect("render to TestBackend should never fail");
    terminal.terminal.backend().buffer().clone()
}

fn popup_terminal_cursor(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Option<CursorState> {
    let popup = app_state.popup_pane.as_ref()?;
    let runtime = terminal_runtimes.get(&popup.terminal_id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let (_, inner) = crate::ui::popup_pane_rects(app_state, app_state.view.terminal_area)?;
    let cursor = runtime.cursor_state(inner, true)?;
    Some(CursorState {
        x: cursor.x,
        y: cursor.y,
        visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
        shape: cursor.shape,
    })
}

pub(crate) fn render_terminal_virtual_reusing(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
    terminal: &mut Option<VirtualTerminal>,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    if terminal
        .as_ref()
        .is_none_or(|terminal| terminal.size != (area.width, area.height))
    {
        *terminal = Some(VirtualTerminal::new(area.width, area.height));
    }
    let terminal = terminal
        .as_mut()
        .expect("virtual terminal was initialized for this client size");
    render_terminal_virtual_using_terminal(runtime, area, terminal)
}

fn render_terminal_virtual_using_terminal(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
    terminal: &mut VirtualTerminal,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let suppress_cursor = runtime.synchronized_output_active();
    terminal.terminal.backend_mut().clear_rendered_cursor();
    terminal
        .terminal
        .draw(|frame| {
            runtime.render(frame, area, true);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.terminal.backend().buffer().clone();
    let cursor = (!suppress_cursor)
        .then(|| runtime.cursor_state(area, true))
        .flatten()
        .map(|cursor| CursorState {
            x: cursor.x,
            y: cursor.y,
            visible: cursor.visible && !crate::ui::pane_is_scrolled_back(runtime),
            shape: cursor.shape,
        })
        .or_else(|| {
            (!suppress_cursor)
                .then(|| terminal.terminal.backend().rendered_cursor())
                .flatten()
        });

    (buffer, cursor)
}

pub(crate) fn visible_hyperlinks(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> Vec<((u16, u16), String, String)> {
    crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, app_state.view.tab_surface())
}

pub(crate) fn visible_hyperlink_spans(app_state: &AppState) -> Vec<crate::protocol::HyperlinkSpan> {
    // The notepad's agent tab carries its link labels as OSC 8 hyperlinks too,
    // so a short label still exposes the full URL to the outer terminal.
    crate::ui::notepad_agent::hyperlink_spans(app_state)
}

fn focused_terminal_owns_host_cursor_when_owned(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some()
}

fn focused_terminal_suppresses_host_cursor_when_owned(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
) -> bool {
    let Some(ws_idx) = app_state.active else {
        return false;
    };
    let Some(info) = app_state
        .view
        .pane_infos
        .iter()
        .find(|info| info.is_focused)
    else {
        return false;
    };
    if !app_state.pane_exposes_host_cursor(ws_idx, info.id) {
        return false;
    }

    app_state
        .runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        .is_some_and(crate::terminal::TerminalRuntime::synchronized_output_active)
}

#[cfg(test)]
mod render_scale_benchmark {
    use std::hint::black_box;
    use std::time::{Duration, Instant, SystemTime};

    use ratatui::layout::Direction;

    use super::*;
    use crate::app::Mode;
    use crate::terminal::TerminalRuntime;
    use crate::workspace::Workspace;

    const AREA: Rect = Rect::new(0, 0, 120, 40);
    const SAMPLE_COUNT: usize = 40;
    const WARMUP_COUNT: usize = 5;

    #[derive(Clone, Copy)]
    struct RenderStats {
        median_us: u128,
        p95_us: u128,
        max_us: u128,
    }

    fn history() -> String {
        (0..2_000).map(|line| format!("line-{line}\r\n")).collect()
    }

    fn runtime(history: &str) -> TerminalRuntime {
        TerminalRuntime::test_with_scrollback_bytes(
            AREA.width,
            AREA.height,
            1024 * 1024,
            history.as_bytes(),
        )
    }

    fn app_with_workspaces(workspace_count: usize) -> AppState {
        let history = history();
        let workspaces = (0..workspace_count)
            .map(|index| {
                let mut workspace = Workspace::test_new(&format!("bench-{}", index + 1));
                let root_pane = workspace.tabs[0].root_pane;
                workspace.tabs[0]
                    .runtimes
                    .insert(root_pane, runtime(&history));
                workspace
            })
            .collect();
        app_with(workspaces)
    }

    fn app_with_working_sessions(workspace_count: usize, opacity: u8) -> AppState {
        let mut app = app_with_workspaces(workspace_count);
        app.ensure_test_terminals();
        for terminal in app.terminals.values_mut() {
            terminal.detected_agent = Some(crate::detect::Agent::Codex);
            terminal.set_raw_agent_state_for_test(crate::detect::AgentState::Working);
        }
        app.reconcile_sidebar_presentation();
        app.working_row_opacity_percent = opacity;
        app
    }

    fn app_with_active_panes(pane_count: usize) -> AppState {
        let history = history();
        let mut workspace = Workspace::test_new("bench");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0]
            .runtimes
            .insert(root_pane, runtime(&history));
        let mut pane_ids = vec![root_pane];

        for index in 1..pane_count {
            let target = pane_ids[(index - 1) / 2];
            workspace.tabs[0].layout.focus_pane(target);
            let direction = if index % 2 == 0 {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let pane_id = workspace.test_split(direction);
            workspace.tabs[0]
                .runtimes
                .insert(pane_id, runtime(&history));
            pane_ids.push(pane_id);
        }

        app_with(vec![workspace])
    }

    fn app_with_remote_agents(remote_count: usize) -> AppState {
        let mut app = app_with_workspaces(1);
        let snapshot = crate::fleet::Snapshot {
            hosts: vec![crate::fleet::HostSnapshot {
                name: "bench-remote".into(),
                target: "bench-remote".into(),
                local: false,
                session: None,
                socket: None,
                state: crate::fleet::HostState::Reachable,
                version: None,
                protocol: None,
                error: None,
                remote_identity: None,
                sessions: None,
                reachable: true,
                last_seen_unix_ms: None,
                entries: (0..remote_count)
                    .map(|index| {
                        let name = format!("remote-{index}");
                        let agent = serde_json::from_value(serde_json::json!({
                            "terminal_id": format!("terminal-{name}"),
                            "name": name,
                            "agent": "codex",
                            "agent_status": "idle",
                            "usage_limited": false,
                            "workspace_id": "workspace",
                            "tab_id": format!("tab-{index}"),
                            "pane_id": format!("pane-{index}"),
                            "focused": false,
                            "revision": 1
                        }))
                        .expect("valid remote benchmark agent");
                        crate::fleet::FleetRow::test_agent_info_row("bench-remote", agent)
                    })
                    .collect(),
            }],
            ..crate::fleet::Snapshot::default()
        };
        app.remote_agent_panel_entries = crate::ui::remote_agent_panel_entries(&snapshot, false);
        app.remote_agent_device_groups = Some(crate::ui::sidebar::remote_agent_device_groups(&app));
        app.sidebar_sections_layout = true;
        app.collapsed_sidebar_groups.remove("repo:Fleet");
        assert_eq!(app.remote_agent_panel_entries.len(), remote_count);
        assert!(crate::ui::sidebar_rows(&app).iter().any(|row| matches!(
            row,
            crate::ui::SidebarRow::NestedHeader { key, count, .. }
                if key.starts_with("device:main/") && *count == remote_count
        )));
        app
    }

    fn proxy_runtime(pane_id: crate::layout::PaneId, history: &str) -> TerminalRuntime {
        let (runtime, _channels) = TerminalRuntime::spawn_remote_proxy(
            pane_id,
            AREA.height,
            AREA.width,
            1024 * 1024,
            std::sync::Arc::new(tokio::sync::Notify::new()),
            std::sync::Arc::new(crate::render_signal::RenderSignal::default()),
            crate::remote::RemoteFocusOperationState::new(),
        )
        .expect("proxy runtime");
        runtime.process_remote_frame(history.as_bytes());
        runtime
    }

    fn app_with_proxy_panes(pane_count: usize) -> AppState {
        let history = history();
        let mut workspace = Workspace::test_new("bench");
        let root_pane = workspace.tabs[0].root_pane;
        workspace.tabs[0]
            .runtimes
            .insert(root_pane, proxy_runtime(root_pane, &history));
        let mut pane_ids = vec![root_pane];

        for index in 1..pane_count {
            let target = pane_ids[(index - 1) / 2];
            workspace.tabs[0].layout.focus_pane(target);
            let direction = if index % 2 == 0 {
                Direction::Vertical
            } else {
                Direction::Horizontal
            };
            let pane_id = workspace.test_split(direction);
            workspace.tabs[0]
                .runtimes
                .insert(pane_id, proxy_runtime(pane_id, &history));
            pane_ids.push(pane_id);
        }

        app_with(vec![workspace])
    }

    fn app_with_agent_tab_links(link_count: usize, open: bool) -> AppState {
        let mut app = app_with_workspaces(1);
        app.ensure_test_terminals();
        let pane_id = app.workspaces[0].focused_pane_id().expect("focused pane");
        let terminal_id = app.workspaces[0]
            .terminal_id(pane_id)
            .expect("pane terminal")
            .clone();
        app.terminals
            .get_mut(&terminal_id)
            .expect("terminal state")
            .set_detected_state(
                Some(crate::detect::Agent::Codex),
                crate::detect::AgentState::Working,
            );
        let suffix = "x".repeat(8 * 1024);
        for index in 0..link_count {
            app.agent_states.observe_links(
                pane_id,
                [format!(
                    "https://github.com/owner/repo/pull/{index}?{suffix}"
                )],
                crate::agent_state::AgentLinkSource::Output,
                SystemTime::UNIX_EPOCH + Duration::from_secs(1_760_000_000 + index as u64),
            );
        }
        app.notepad.enabled = true;
        app.notepad.height = 18;
        if open {
            app.notepad
                .set_visible_tabs(vec!["context".into(), "agent".into()]);
            assert!(app.notepad.select_agent_tab());
        }
        app
    }

    fn app_with_usage_tab_panes(pane_count: usize, open: bool) -> AppState {
        let mut app = app_with_active_panes(pane_count);
        app.notepad.enabled = true;
        app.notepad.height = 18;
        app.provider_usage = crate::provider_usage::ProviderUsageSnapshot::with_primary_accounts(
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
            crate::provider_usage::AccountUsage::default(),
        );
        if open {
            app.notepad
                .set_visible_tabs(vec!["context".into(), "usage".into()]);
            assert!(app.notepad.select_usage_tab());
        }
        app
    }

    fn app_with_pods(pane_count: usize) -> AppState {
        let mut app = app_with_active_panes(pane_count);
        app.ensure_test_terminals();
        app.refresh_local_agent_panel_identities();
        let local = crate::groups::AuthorityId::from_random_bytes([61; 16]);
        let remote = crate::groups::AuthorityId::from_random_bytes([62; 16]);
        let ids = [
            crate::groups::GroupId {
                owner: local.clone(),
                local: 1,
            },
            crate::groups::GroupId {
                owner: local.clone(),
                local: 2,
            },
            crate::groups::GroupId {
                owner: remote.clone(),
                local: 1,
            },
        ];
        let memberships = app
            .local_agent_panel_identities
            .values()
            .enumerate()
            .map(|(index, identity)| crate::groups::OwnedPaneMembership {
                pane_id: identity.agent_ref.agent.clone(),
                pane_incarnation: format!("pane-{index}"),
                membership: crate::groups::PaneGroupMembership {
                    group_id: Some(ids[index % ids.len()].clone()),
                    revision: 1,
                },
            })
            .collect();
        app.local_group_snapshot = Some(crate::groups::GroupAuthoritySnapshot {
            authority_id: local.clone(),
            revision: 2,
            groups: ids[..2]
                .iter()
                .enumerate()
                .map(|(index, id)| crate::groups::GroupRecord {
                    id: id.clone(),
                    revision: 1,
                    state: crate::groups::GroupState::Active {
                        name: format!("local-pod-{index}"),
                    },
                })
                .collect(),
            memberships,
        });
        app.fleet_snapshot.group_catalogs = vec![crate::fleet::GroupCatalog {
            host: "bench-remote".into(),
            target: "bench-remote".into(),
            local: false,
            session: None,
            socket: None,
            state: crate::fleet::GroupCatalogState::Stale,
            observed_authority_id: Some(remote.clone()),
            snapshot: Some(crate::groups::GroupAuthoritySnapshot {
                authority_id: remote,
                revision: 1,
                groups: vec![crate::groups::GroupRecord {
                    id: ids[2].clone(),
                    revision: 1,
                    state: crate::groups::GroupState::Active {
                        name: "remote-stale".into(),
                    },
                }],
                memberships: Vec::new(),
            }),
            error: Some("benchmark stale catalog".into()),
        }];
        app
    }

    fn app_with(workspaces: Vec<Workspace>) -> AppState {
        let mut app = AppState::test_new();
        app.set_server_mode(Mode::Terminal);
        app.pane_scrollbars = true;
        app.workspaces = workspaces;
        app.refresh_local_agent_panel_identities();
        app.active = Some(0);
        app.selected = 0;
        app
    }

    #[tokio::test]
    async fn client_overlay_caret_owns_the_host_cursor_over_the_terminal() {
        let mut app = app_with_workspaces(1);
        let workspace_id = app.workspaces[0].id.clone();
        let tab_id = crate::workspace::public_tab_id_for_number(
            &workspace_id,
            app.workspaces[0].tabs[0].number,
        );
        app.open_client_overlay(crate::app::state::ClientOverlay::RenameTab);
        app.rename_target = Some(crate::app::state::RenameTarget::Tab {
            workspace_id,
            tab_id,
        });
        app.name_input = "ab".into();

        let (buffer, cursor) = render_virtual(&mut app, AREA, true);
        let cursor = cursor.expect("rename caret");

        assert!(cursor.visible);
        assert!(cursor.x > 0);
        assert_eq!(buffer[(cursor.x - 1, cursor.y)].symbol(), "b");
        assert_eq!(buffer[(cursor.x, cursor.y)].symbol(), " ");
    }

    fn profile(mut app: AppState) -> RenderStats {
        for _ in 0..WARMUP_COUNT {
            black_box(render_virtual(&mut app, AREA, true));
        }

        let mut samples = Vec::with_capacity(SAMPLE_COUNT);
        for _ in 0..SAMPLE_COUNT {
            let started = Instant::now();
            black_box(render_virtual(&mut app, AREA, true));
            samples.push(started.elapsed().as_micros());
        }
        samples.sort_unstable();

        RenderStats {
            median_us: samples[SAMPLE_COUNT / 2],
            p95_us: samples[(SAMPLE_COUNT - 1) * 95 / 100],
            max_us: samples[SAMPLE_COUNT - 1],
        }
    }

    fn profile_cardinalities(build: fn(usize) -> AppState) -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(build(count))))
    }

    fn profile_working_cardinalities(opacity: u8) -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_working_sessions(count, opacity))))
    }

    fn profile_remote_cardinalities() -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_remote_agents(count))))
    }

    fn profile_proxy_cardinalities() -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_proxy_panes(count))))
    }

    fn profile_agent_tab_cardinalities(open: bool) -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_agent_tab_links(count, open))))
    }

    fn profile_usage_tab_cardinalities(open: bool) -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_usage_tab_panes(count, open))))
    }

    fn profile_pod_cardinalities() -> [(usize, RenderStats); 3] {
        [1, 15, 50].map(|count| (count, profile(app_with_pods(count))))
    }

    fn print_profiles(label: &str, profiles: [(usize, RenderStats); 3]) {
        let baseline_median_us = profiles[0].1.median_us as f64;
        let baseline_p95_us = profiles[0].1.p95_us as f64;
        println!("{label}");
        println!("     count  median_us  p95_us  max_us  median_vs_1x  p95_vs_1x");
        for (count, stats) in profiles {
            println!(
                "{count:>10}  {:>9}  {:>6}  {:>6}  {:>12.2}  {:>9.2}",
                stats.median_us,
                stats.p95_us,
                stats.max_us,
                stats.median_us as f64 / baseline_median_us,
                stats.p95_us as f64 / baseline_p95_us,
            );
        }
    }

    fn assert_full_render_avoids_aggregate_input_state(mut app: AppState, scenario: &str) {
        crate::pane::reset_aggregate_input_state_reads();
        black_box(render_virtual(&mut app, AREA, true));
        assert_eq!(
            crate::pane::aggregate_input_state_reads(),
            0,
            "full render collected aggregate input state for {scenario}",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn aggregate_input_state_counter_records_reads() {
        let runtime = TerminalRuntime::test_with_screen_bytes(80, 24, b"");
        crate::pane::reset_aggregate_input_state_reads();
        black_box(runtime.input_state());
        assert_eq!(crate::pane::aggregate_input_state_reads(), 1);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn full_render_avoids_aggregate_input_state_reads() {
        assert_full_render_avoids_aggregate_input_state(
            app_with_workspaces(15),
            "background workspaces",
        );
        assert_full_render_avoids_aggregate_input_state(app_with_active_panes(15), "active panes");
        assert_full_render_avoids_aggregate_input_state(
            app_with_proxy_panes(15),
            "remote focus proxy panes",
        );
    }

    #[tokio::test(flavor = "current_thread")]
    async fn remote_render_scale_fixture_builds_requested_rows() {
        for count in [1, 15, 50] {
            black_box(app_with_remote_agents(count));
        }
    }

    #[tokio::test(flavor = "current_thread")]
    async fn agent_tab_benchmark_fixture_really_opens_and_closes_the_tab() {
        assert!(app_with_agent_tab_links(15, true).notepad.agent_tab);
        assert!(!app_with_agent_tab_links(15, false).notepad.agent_tab);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn usage_tab_benchmark_fixture_really_opens_and_closes_the_tab() {
        assert!(app_with_usage_tab_panes(15, true).notepad.usage_tab);
        assert!(!app_with_usage_tab_panes(15, false).notepad.usage_tab);
    }

    #[tokio::test(flavor = "current_thread")]
    async fn pod_profile_fixture_has_three_pods_two_hosts_and_one_stale() {
        let app = app_with_pods(15);
        let rows = crate::ui::sidebar_rows(&app);
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, crate::ui::SidebarRow::PodHeader { .. }))
                .count(),
            3
        );
        assert_eq!(
            rows.iter()
                .filter_map(|row| match row {
                    crate::ui::SidebarRow::PodHeader { group_id, .. } => {
                        Some(group_id.owner.clone())
                    }
                    _ => None,
                })
                .collect::<std::collections::HashSet<_>>()
                .len(),
            2
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, crate::ui::SidebarRow::PodHeader { fresh: false, .. }))
                .count(),
            1
        );
        assert_eq!(
            rows.iter()
                .filter(|row| matches!(row, crate::ui::SidebarRow::PodMember { .. }))
                .count(),
            15
        );
    }

    #[test]
    fn unchanged_home_composer_renders_identical_consecutive_frames() {
        let mut app = AppState::test_new();
        app.home = Some(crate::app::home::HomeState::default());

        let first = render_virtual(&mut app, AREA, false);
        let second = render_virtual(&mut app, AREA, false);

        assert_eq!(first, second);
    }

    #[tokio::test(flavor = "current_thread")]
    #[ignore = "manual full-render scaling profile"]
    async fn render_scale_profile() {
        print_profiles(
            "blue-dot Working sessions at 100% opacity (one pane each)",
            profile_working_cardinalities(100),
        );
        print_profiles(
            "blue-dot Working sessions at 25% opacity (one pane each)",
            profile_working_cardinalities(25),
        );
        print_profiles(
            "background-workspace resize/layout (one pane each)",
            profile_cardinalities(app_with_workspaces),
        );
        print_profiles(
            "active panes (one workspace)",
            profile_cardinalities(app_with_active_panes),
        );
        print_profiles(
            "remote agents (one local pane)",
            profile_remote_cardinalities(),
        );
        print_profiles(
            "remote focus proxy panes (one workspace)",
            profile_proxy_cardinalities(),
        );
        print_profiles(
            "agent tab closed (8 KiB retained links)",
            profile_agent_tab_cardinalities(false),
        );
        print_profiles(
            "agent tab open (8 KiB retained links)",
            profile_agent_tab_cardinalities(true),
        );
        print_profiles(
            "usage tab closed (three accounts)",
            profile_usage_tab_cardinalities(false),
        );
        print_profiles(
            "usage tab open (three accounts)",
            profile_usage_tab_cardinalities(true),
        );
        print_profiles(
            "Pods section (3 pods, 2 hosts, 1 stale)",
            profile_pod_cardinalities(),
        );
    }
}
