use std::collections::{HashMap, VecDeque};
use std::path::PathBuf;

use crate::protocol::RenderEncoding;
use crate::server::client_transport::ClientWriter;
use crate::server::render_stream::ClientRenderState;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum ClientConnectionMode {
    App,
    TerminalAttach {
        terminal_id: String,
        control: Option<Box<crate::server::remote_control::RemoteControlLease>>,
    },
    TerminalObserve {
        terminal_id: String,
    },
}

pub(crate) type RenderTarget = (
    u64,
    (u16, u16),
    crate::kitty_graphics::HostCellSize,
    bool,
    ClientConnectionMode,
);

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct ClientTerminalGeometry {
    pub(crate) cols: u16,
    pub(crate) rows: u16,
    pub(crate) cell_size: crate::kitty_graphics::HostCellSize,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(crate) enum DeferredRender {
    #[default]
    None,
    Full,
}

/// A connected client tracked by the server.
pub(crate) struct ClientConnection {
    /// Whether this connection is the full app client or a direct terminal attach.
    pub(crate) mode: ClientConnectionMode,
    /// True after the handshake for clients that will switch into direct terminal attach mode.
    pub(crate) pending_terminal_attach: bool,
    /// Client-local app keybindings. None means use the server's keybindings.
    pub(crate) keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
    /// The client's terminal size after clamping.
    pub(crate) terminal_size: (u16, u16),
    /// Pixel size of one client terminal cell.
    pub(crate) cell_size: crate::kitty_graphics::HostCellSize,
    /// Last known host terminal default colors for this client.
    pub(crate) host_terminal_theme: crate::terminal_theme::TerminalTheme,
    /// Last known host terminal appearance for this client.
    pub(crate) host_terminal_appearance: Option<crate::terminal_theme::HostAppearance>,
    /// True when appearance came from an explicit host color-scheme report.
    pub(crate) host_terminal_appearance_explicit: bool,
    /// Last reported focus state for this client's outer terminal.
    pub(crate) outer_terminal_focus: Option<bool>,
    /// Stateful parser for app-client input split across transport reads.
    pub(crate) raw_input: crate::raw_input::RawInputFramer,
    /// Monotonic activity stamp used to choose the fallback foreground client.
    pub(crate) last_activity: u64,
    /// Render baseline for the negotiated client encoding.
    pub(crate) render_state: ClientRenderState,
    /// Ratatui buffers for this client's full virtual terminal.
    pub(crate) render_terminal: Option<crate::server::render_stream::VirtualTerminal>,
    /// Small retained terminal used to redraw only the sidebar animation.
    pub(crate) animation_terminal: Option<crate::server::render_stream::VirtualTerminal>,
    /// Sidebar animation area from this client's last full layout.
    pub(crate) animation_rect: ratatui::layout::Rect,
    /// Pomodoro input ownership from the last frame plus overlays raised since it.
    pub(crate) pomodoro_presentation: crate::ui::pomodoro::InputPresentation,
    /// Pomodoro input ownership awaiting transport flush acknowledgements.
    pending_pomodoro_presentations: VecDeque<(u64, crate::ui::pomodoro::InputPresentation)>,
    /// Sidebar disclosure, projection escape, and scroll state for this attach.
    pub(crate) sidebar_presentation: crate::app::state::SidebarPresentationState,
    /// Dock layout and focus state for this attach; editor PTYs are server-owned.
    pub(crate) dock_presentation: crate::app::state::DockPresentationState,
    /// Selected notepad projection, folds, and scroll for this attach.
    pub(crate) notepad_presentation: crate::notepad::NotepadPresentationState,
    /// Client-local run-history detail surface, separate from server-owned receipt facts.
    pub(crate) loop_run_history_detail: Option<crate::app::state::LoopRunHistoryDetail>,
    /// Client-local aloop run-log surface (MAT-159 AC7); the run records
    /// themselves are server-owned in the fleet snapshot.
    pub(crate) aloop_run_detail: Option<crate::app::state::AloopRunDetail>,
    /// Client-local Symphony dashboard selection over the server-owned snapshot.
    pub(crate) symphony_detail: Option<crate::app::state::SymphonyDetail>,
    /// Client-local work projection view over the server-owned work index snapshot.
    pub(crate) work_view: Option<crate::app::state::WorkViewState>,
    /// Client-local historical provider usage controls and scan snapshot.
    pub(crate) usage_view: Option<crate::app::state::UsageViewState>,
    /// Last tiled-pane geometry rendered for this client. Capacity is reused across frames.
    pub(crate) retained_pane_infos: std::sync::Arc<Vec<crate::layout::PaneInfo>>,
    /// The client resized after its attach-local presentation was last used to
    /// compute input hit areas.
    pub(crate) input_geometry_dirty: bool,
    /// Last terminal viewport geometry computed for this client.
    pub(crate) terminal_geometries: HashMap<crate::terminal::TerminalId, ClientTerminalGeometry>,
    /// Whether pane PTY output owns this client's cursor on its last full frame.
    pub(crate) retained_pane_cursor: bool,
    /// Client-local host Kitty graphics cache.
    pub(crate) graphics_cache: crate::kitty_graphics::HostGraphicsCache,
    /// Passive eligibility for audited local Kitty regular-file graphics.
    pub(crate) direct_graphics: bool,
    /// Host rejected terminal frame files; pane image files remain available.
    #[cfg(unix)]
    pub(crate) terminal_file_graphics: bool,
    /// The single terminal-image transfer awaiting the local host response.
    #[cfg(unix)]
    pub(crate) direct_terminal_graphics: Option<crate::kitty_graphics::DirectTerminalTransfer>,
    /// Whether this frontend preserves exact SGR pixel reports.
    pub(crate) pixel_mouse: bool,
    /// Whether the next graphics frame must clear and rebuild host-side Kitty state.
    pub(crate) graphics_surface_reset_pending: bool,
    /// Whether an ordinary render was skipped because the render channel was full.
    pub(crate) render_pending: bool,
    /// Last host mouse capture mode sent to this client.
    pub(crate) host_mouse_capture_active: Option<bool>,
    /// Last SGR pixel provenance mode sent to this client.
    pub(crate) host_sgr_pixels_active: Option<bool>,
    /// Last Kitty report-all mode sent to this client's host terminal.
    pub(crate) host_keyboard_report_all_active: Option<bool>,
    /// Temporary files staged from this client's local clipboard image pastes.
    pub(crate) staged_clipboard_files: Vec<PathBuf>,
    /// Channels for sending framed ServerMessage data to the client writer thread.
    pub(crate) writer: Option<ClientWriter>,
}

impl ClientConnection {
    #[cfg(test)]
    pub(crate) fn new(
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self::new_with_mode(
            ClientConnectionMode::App,
            None,
            terminal_size,
            cell_size,
            host_terminal_theme,
            outer_terminal_focus,
            last_activity,
            render_encoding,
            false,
            writer,
        )
    }

    pub(crate) fn new_with_mode(
        mode: ClientConnectionMode,
        keybindings: Option<Box<crate::config::LiveKeybindConfig>>,
        terminal_size: (u16, u16),
        cell_size: crate::kitty_graphics::HostCellSize,
        host_terminal_theme: crate::terminal_theme::TerminalTheme,
        outer_terminal_focus: Option<bool>,
        last_activity: u64,
        render_encoding: RenderEncoding,
        pending_terminal_attach: bool,
        writer: Option<ClientWriter>,
    ) -> Self {
        Self {
            mode,
            pending_terminal_attach,
            keybindings,
            terminal_size,
            cell_size,
            host_terminal_appearance: host_terminal_theme
                .background
                .map(crate::terminal_theme::RgbColor::inferred_appearance),
            host_terminal_appearance_explicit: false,
            host_terminal_theme,
            outer_terminal_focus,
            raw_input: crate::raw_input::RawInputFramer::default(),
            last_activity,
            render_state: ClientRenderState::new(render_encoding),
            render_terminal: None,
            animation_terminal: None,
            animation_rect: ratatui::layout::Rect::default(),
            pomodoro_presentation: crate::ui::pomodoro::InputPresentation::default(),
            pending_pomodoro_presentations: VecDeque::new(),
            sidebar_presentation: crate::app::state::SidebarPresentationState::default(),
            dock_presentation: crate::app::state::DockPresentationState::default(),
            notepad_presentation: crate::notepad::NotepadPresentationState::default(),
            loop_run_history_detail: None,
            aloop_run_detail: None,
            symphony_detail: None,
            work_view: None,
            usage_view: None,
            retained_pane_infos: std::sync::Arc::new(Vec::new()),
            input_geometry_dirty: false,
            terminal_geometries: HashMap::new(),
            retained_pane_cursor: false,
            graphics_cache: crate::kitty_graphics::HostGraphicsCache::default(),
            direct_graphics: false,
            #[cfg(unix)]
            terminal_file_graphics: true,
            #[cfg(unix)]
            direct_terminal_graphics: None,
            pixel_mouse: false,
            graphics_surface_reset_pending: false,
            render_pending: false,
            host_mouse_capture_active: None,
            host_sgr_pixels_active: None,
            host_keyboard_report_all_active: None,
            staged_clipboard_files: Vec::new(),
            writer,
        }
    }

    pub(crate) fn request_repaint(&mut self) {
        self.render_state.request_repaint();
    }

    pub(crate) fn queue_pomodoro_presentation(
        &mut self,
        render_sequence: u64,
        presentation: crate::ui::pomodoro::InputPresentation,
    ) {
        let already_owned = self.pomodoro_presentation.owns_input();
        self.pomodoro_presentation.send_off = self
            .pomodoro_presentation
            .send_off
            .or(presentation.send_off);
        self.pomodoro_presentation.prompt =
            self.pomodoro_presentation.prompt.or(presentation.prompt);
        if !already_owned && self.pomodoro_presentation.owns_input() {
            self.pomodoro_presentation.area = presentation.area;
        }
        self.pending_pomodoro_presentations
            .push_back((render_sequence, presentation));
    }

    pub(crate) fn take_flushed_pomodoro_presentation(
        &mut self,
        render_sequence: u64,
    ) -> Option<crate::ui::pomodoro::InputPresentation> {
        let mut flushed = None;
        while self
            .pending_pomodoro_presentations
            .front()
            .is_some_and(|(sequence, _)| *sequence <= render_sequence)
        {
            let Some((sequence, presentation)) = self.pending_pomodoro_presentations.pop_front()
            else {
                break;
            };
            if sequence == render_sequence {
                flushed = Some(presentation);
            }
        }
        flushed
    }

    #[cfg(test)]
    pub(crate) fn latest_pending_pomodoro_render_sequence(&self) -> Option<u64> {
        self.pending_pomodoro_presentations
            .back()
            .map(|(sequence, _)| *sequence)
    }

    pub(crate) fn deferred_render(&self) -> DeferredRender {
        if self.render_pending {
            DeferredRender::Full
        } else {
            DeferredRender::None
        }
    }

    pub(crate) fn clear_deferred_render(&mut self) {
        self.render_pending = false;
    }

    pub(crate) fn defer_full_render(&mut self) {
        self.render_pending = true;
    }

    pub(crate) fn take_deferred_render(&mut self) -> DeferredRender {
        let deferred = self.deferred_render();
        self.clear_deferred_render();
        deferred
    }

    pub(crate) fn is_full_app_client(&self) -> bool {
        matches!(self.mode, ClientConnectionMode::App) && !self.pending_terminal_attach
    }

    pub(crate) fn presentation_policy(
        &self,
        app_state: &crate::app::state::AppState,
    ) -> crate::app::state::ClientPresentationPolicy<'static> {
        self.presentation_policy_with_pomodoro(app_state, self.pomodoro_presentation.owns_input())
    }

    pub(crate) fn presentation_policy_with_pomodoro(
        &self,
        app_state: &crate::app::state::AppState,
        pomodoro_owns_input: bool,
    ) -> crate::app::state::ClientPresentationPolicy<'static> {
        let dock = &self.dock_presentation;
        let preview_is_in_dock =
            !dock.collapsed && dock.tab == Some(crate::app::DockSurface::Editor);
        let tab_surface_replaced = (dock.editor_preview.is_some() && !preview_is_in_dock)
            || self.symphony_detail.is_some()
            || self.loop_run_history_detail.is_some()
            || self.aloop_run_detail.is_some()
            || self.usage_view.is_some()
            || self.work_view.is_some()
            || (dock.collapsed && dock.object_preview.is_some())
            || app_state.home.is_some()
            || app_state.inbox.is_some()
            || app_state
                .active
                .and_then(|ws_idx| app_state.workspaces.get(ws_idx))
                .is_none();
        crate::app::state::ClientPresentationPolicy::from_presentations(
            app_state.server_mode(),
            &self.sidebar_presentation,
            &self.dock_presentation,
            pomodoro_owns_input,
            !tab_surface_replaced,
            std::sync::Arc::clone(&self.retained_pane_infos),
        )
    }

    pub(crate) fn request_semantic_redraw_after_input(&mut self) {
        self.render_state.reset_semantic_input_baseline();
    }

    pub(crate) fn update_host_theme_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> bool {
        let mut next_theme = self.host_terminal_theme;
        let mut changed = false;
        for event in events {
            match event {
                crate::raw_input::RawInputEvent::HostDefaultColor { kind, color } => {
                    next_theme = next_theme.with_color(*kind, *color);
                    if matches!(kind, crate::terminal_theme::DefaultColorKind::Background)
                        && !self.host_terminal_appearance_explicit
                    {
                        changed |=
                            self.set_host_appearance(Some(color.inferred_appearance()), false);
                    }
                }
                crate::raw_input::RawInputEvent::HostPaletteColors { colors } => {
                    for &(index, color) in colors {
                        next_theme = next_theme.with_palette_color(index, color);
                    }
                }
                crate::raw_input::RawInputEvent::HostColorSchemeChanged(appearance) => {
                    changed |= self.set_host_appearance(Some(*appearance), true);
                }
                _ => {}
            }
        }

        if next_theme != self.host_terminal_theme {
            self.host_terminal_theme = next_theme;
            changed = true;
        }
        changed
    }

    fn set_host_appearance(
        &mut self,
        appearance: Option<crate::terminal_theme::HostAppearance>,
        explicit: bool,
    ) -> bool {
        if self.host_terminal_appearance_explicit && !explicit {
            return false;
        }
        if self.host_terminal_appearance == appearance
            && self.host_terminal_appearance_explicit == explicit
        {
            return false;
        }
        self.host_terminal_appearance = appearance;
        self.host_terminal_appearance_explicit = explicit;
        true
    }

    pub(crate) fn update_outer_focus_from_events(
        &mut self,
        events: &[crate::raw_input::RawInputEvent],
    ) -> Option<bool> {
        let next_focus = events
            .iter()
            .filter_map(|event| match event {
                crate::raw_input::RawInputEvent::OuterFocusGained => Some(true),
                crate::raw_input::RawInputEvent::OuterFocusLost => Some(false),
                _ => None,
            })
            .next_back()?;

        self.outer_terminal_focus = Some(next_focus);
        Some(next_focus)
    }
}

pub(crate) fn events_include_interaction(events: &[crate::raw_input::RawInputEvent]) -> bool {
    events.iter().any(|event| {
        matches!(
            event,
            crate::raw_input::RawInputEvent::Key(_)
                | crate::raw_input::RawInputEvent::Text(_)
                | crate::raw_input::RawInputEvent::Mouse(_)
                | crate::raw_input::RawInputEvent::Paste(_)
                | crate::raw_input::RawInputEvent::OuterFocusGained
        )
    })
}

pub(crate) fn latest_app_client(clients: &HashMap<u64, ClientConnection>) -> Option<u64> {
    clients
        .iter()
        .filter(|(_, client)| client.is_full_app_client())
        .max_by_key(|(_, client)| client.last_activity)
        .map(|(&client_id, _)| client_id)
}

/// Focus reporting is optional. An attached client that has not reported focus
/// must preserve the pre-reporting behaviour and count as focused.
pub(crate) fn aggregate_outer_focus(reports: impl IntoIterator<Item = Option<bool>>) -> bool {
    reports.into_iter().any(|focused| focused != Some(false))
}

pub(crate) fn terminal_stream_client_ids(
    clients: &HashMap<u64, ClientConnection>,
    terminal_id: &str,
) -> Vec<u64> {
    clients
        .iter()
        .filter_map(|(&client_id, client)| match &client.mode {
            ClientConnectionMode::TerminalAttach {
                terminal_id: attached,
                ..
            }
            | ClientConnectionMode::TerminalObserve {
                terminal_id: attached,
            } if attached == terminal_id => Some(client_id),
            _ => None,
        })
        .collect()
}

pub(crate) fn render_targets(
    clients: &HashMap<u64, ClientConnection>,
    foreground_client_id: Option<u64>,
) -> Vec<RenderTarget> {
    let mut targets: Vec<RenderTarget> = clients
        .iter()
        .filter(|(_, client)| {
            client.writer.is_some()
                && (client.is_full_app_client()
                    || matches!(
                        client.mode,
                        ClientConnectionMode::TerminalAttach { .. }
                            | ClientConnectionMode::TerminalObserve { .. }
                    ))
        })
        .map(|(&client_id, client)| {
            (
                client_id,
                client.terminal_size,
                client.cell_size,
                foreground_client_id == Some(client_id),
                client.mode.clone(),
            )
        })
        .collect();

    targets.sort_by_key(|(client_id, _, _, is_foreground, _)| (*is_foreground, *client_id));
    targets
}

#[cfg(test)]
mod focus_tests {
    use super::aggregate_outer_focus;

    #[test]
    fn f2_focus_aggregation_accepts_any_focused_or_unknown_client() {
        assert!(aggregate_outer_focus([Some(false), Some(true)]));
        assert!(aggregate_outer_focus([Some(false), None]));
        assert!(!aggregate_outer_focus([Some(false), Some(false)]));
        assert!(!aggregate_outer_focus([]));
    }
}
