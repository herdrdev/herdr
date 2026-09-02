//! Virtual rendering helpers for headless client frame streaming.

use ratatui::backend::{Backend, ClearType, TestBackend, WindowSize};
use ratatui::layout::{Position, Rect, Size};

use crate::app::state::AppState;
use crate::protocol::render_ansi::{BlitEncoder, EncodedBlit};
use crate::protocol::{
    CursorState, FrameData, PaneSurfaceFrame, PaneSurfacePatch, RenderEncoding, ServerMessage,
    TerminalFrame,
};
use crate::terminal::TerminalRuntimeRegistry;

/// Per-client render baseline for the negotiated render encoding.
pub(crate) enum ClientRenderState {
    /// Semantic clients compare full frame data and skip identical frames.
    Semantic {
        last_surface: Option<Box<PaneSurfaceFrame>>,
        surface_revision: u64,
    },
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
            RenderEncoding::SemanticFrame => Self::Semantic {
                last_surface: None,
                surface_revision: 0,
            },
            RenderEncoding::TerminalAnsi => Self::TerminalAnsi {
                blit_encoder: BlitEncoder::new(),
                seq: 0,
                repaint_pending: false,
            },
        }
    }

    pub(crate) fn reset_baseline(&mut self) {
        match self {
            Self::Semantic { last_surface, .. } => *last_surface = None,
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
            Self::Semantic { last_surface, .. } => *last_surface = None,
            Self::TerminalAnsi {
                repaint_pending, ..
            } => *repaint_pending = true,
        }
    }

    pub(crate) fn prepare_frame(&mut self, frame: FrameData) -> Option<PreparedRender> {
        match self {
            Self::Semantic { .. } => None,
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

    pub(crate) fn last_pane_surface(&self) -> Option<&PaneSurfaceFrame> {
        match self {
            Self::Semantic { last_surface, .. } => last_surface.as_deref(),
            Self::TerminalAnsi { .. } => None,
        }
    }

    pub(crate) fn prepare_pane_surface(
        &mut self,
        mut surface: PaneSurfaceFrame,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
        } = self
        else {
            return None;
        };
        if surface.graphics.assets.is_empty()
            && last_surface.as_deref().is_some_and(|last| {
                last.projection_revision == surface.projection_revision
                    && last.frame == surface.frame
                    && last.panes == surface.panes
                    && last.splits == surface.splits
                    && last.popup == surface.popup
                    && last.graphics.placements == surface.graphics.placements
                    && last.graphics.retained_assets == surface.graphics.retained_assets
            })
        {
            return None;
        }
        surface.surface_revision = surface_revision.saturating_add(1);
        let mut committed_surface = surface.clone();
        committed_surface.graphics.assets.clear();
        Some(PreparedRender::Semantic {
            message: ServerMessage::PaneSurface(surface),
            committed_surface: Box::new(committed_surface),
        })
    }

    pub(crate) fn prepare_pane_surface_patch(
        &mut self,
        mut patch: PaneSurfacePatch,
        mut committed_surface: PaneSurfaceFrame,
    ) -> Option<PreparedRender> {
        let Self::Semantic {
            last_surface,
            surface_revision,
        } = self
        else {
            return None;
        };
        let last = last_surface.as_deref()?;
        if last.boot_id != patch.boot_id
            || last.projection_revision != patch.projection_revision
            || last.surface_revision != patch.base_surface_revision
        {
            return None;
        }
        let next_revision = surface_revision.saturating_add(1);
        patch.surface_revision = next_revision;
        committed_surface.surface_revision = next_revision;
        committed_surface.graphics.assets.clear();
        Some(PreparedRender::Semantic {
            message: ServerMessage::PaneSurfacePatch(patch),
            committed_surface: Box::new(committed_surface),
        })
    }

    pub(crate) fn commit_sent_frame(&mut self, prepared: PreparedRender) {
        match (self, prepared) {
            (
                Self::Semantic {
                    last_surface,
                    surface_revision,
                },
                PreparedRender::Semantic {
                    committed_surface, ..
                },
            ) => {
                *surface_revision = committed_surface.surface_revision;
                *last_surface = Some(committed_surface);
            }
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

/// A prepared client render message plus any baseline state needed after send.
pub(crate) enum PreparedRender {
    Semantic {
        message: ServerMessage,
        committed_surface: Box<PaneSurfaceFrame>,
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
            Self::Semantic { message, .. } | Self::TerminalAnsi { message, .. } => message,
        }
    }

    pub(crate) fn strip_pane_surface_assets(&mut self) -> bool {
        let Self::Semantic {
            message: ServerMessage::PaneSurface(surface),
            ..
        } = self
        else {
            return false;
        };
        if surface.graphics.assets.is_empty() {
            return false;
        }
        surface.graphics.assets.clear();
        true
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

pub(crate) type RenderedTabSurface = (
    ratatui::buffer::Buffer,
    Option<CursorState>,
    Vec<((u16, u16), String, String)>,
    crate::ui::TabSurfaceLayout,
);

/// Renders only the active tab's pane surface at an origin-relative client viewport.
pub(crate) fn render_tab_surface_virtual(
    app_state: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    target: Option<crate::ui::TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> RenderedTabSurface {
    let layout = crate::ui::compute_tab_surface_for(
        app_state,
        terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    );
    let surface = crate::ui::TabSurfaceView {
        target: layout.target,
        pane_infos: &layout.pane_infos,
        split_borders: &layout.split_borders,
    };
    let cursor = crate::ui::tab_surface_cursor(app_state, terminal_runtimes, surface);
    let hyperlinks = crate::ui::tab_surface_hyperlinks(app_state, terminal_runtimes, surface);

    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");
    terminal
        .draw(|frame| {
            crate::ui::render_tab_surface(app_state, terminal_runtimes, surface, frame);
        })
        .expect("render to TestBackend should never fail");

    (
        terminal.backend().buffer().clone(),
        cursor,
        hyperlinks,
        layout,
    )
}

/// Renders one server-owned terminal directly for `terminal attach` clients.
pub(crate) fn render_terminal_virtual(
    runtime: &crate::terminal::TerminalRuntime,
    area: Rect,
) -> (ratatui::buffer::Buffer, Option<CursorState>) {
    let suppress_cursor = runtime.synchronized_output_active();
    let backend = CursorTrackingBackend::new(area.width, area.height);
    let mut terminal = ratatui::Terminal::new(backend).expect("TestBackend::new should never fail");

    terminal
        .draw(|frame| {
            runtime.render(frame, area, true);
        })
        .expect("render to TestBackend should never fail");

    let buffer = terminal.backend().buffer().clone();
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
                .then(|| terminal.backend().rendered_cursor())
                .flatten()
        });

    (buffer, cursor)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::protocol::ClientShellPopupSurface;

    fn popup_surface(content: &str) -> PaneSurfaceFrame {
        let pane = ratatui::buffer::Buffer::with_lines(["pane"]);
        let popup = ratatui::buffer::Buffer::with_lines([content]);
        PaneSurfaceFrame {
            boot_id: "boot-1".into(),
            projection_revision: 1,
            surface_revision: 1,
            frame: FrameData::from_ratatui_buffer_with_hyperlinks(&pane, None, &[]),
            panes: Vec::new(),
            splits: Vec::new(),
            popup: Some(Box::new(ClientShellPopupSurface {
                terminal_id: "popup-terminal".into(),
                title: "popup".into(),
                width: None,
                height: None,
                frame: FrameData::from_ratatui_buffer_with_hyperlinks(&popup, None, &[]),
                mouse_reporting: false,
                sgr_pixel_mouse: false,
                pixel_width: 0,
                pixel_height: 0,
            })),
            graphics: crate::protocol::SurfaceGraphicsScene::default(),
        }
    }

    #[test]
    fn popup_only_surface_changes_are_not_deduplicated() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state
            .prepare_pane_surface(popup_surface("first"))
            .expect("initial surface");
        state.commit_sent_frame(prepared);

        assert!(state
            .prepare_pane_surface(popup_surface("second"))
            .is_some());
    }

    #[test]
    fn forced_full_surface_keeps_the_connection_revision_monotonic() {
        let mut state = ClientRenderState::new(RenderEncoding::SemanticFrame);
        let prepared = state
            .prepare_pane_surface(popup_surface("first"))
            .expect("initial surface");
        state.commit_sent_frame(prepared);
        state.request_repaint();

        let prepared = state
            .prepare_pane_surface(popup_surface("replacement"))
            .expect("forced replacement surface");
        assert!(matches!(
            prepared.message(),
            ServerMessage::PaneSurface(surface) if surface.surface_revision == 2
        ));
        state.commit_sent_frame(prepared);
        assert_eq!(state.last_pane_surface().unwrap().surface_revision, 2);
    }
}
