use ratatui::{layout::Rect, Frame};

use super::panes::{compute_pane_infos_for_tab, render_panes, resize_tab_panes};
use crate::app::AppState;
use crate::layout::{PaneInfo, SplitBorder};
use crate::protocol::CursorState;
use crate::terminal::TerminalRuntimeRegistry;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) struct TabSurfaceTarget {
    pub(crate) workspace_index: usize,
    pub(crate) tab_index: usize,
}

pub(crate) struct TabSurfaceLayout {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: Vec<PaneInfo>,
    pub(crate) split_borders: Vec<SplitBorder>,
}

#[derive(Clone, Copy)]
pub(crate) struct TabSurfaceView<'a> {
    pub(crate) target: Option<TabSurfaceTarget>,
    pub(crate) pane_infos: &'a [PaneInfo],
    pub(crate) split_borders: &'a [SplitBorder],
}

pub(crate) fn compute_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let target = app.active.and_then(|workspace_index| {
        let workspace = app.workspaces.get(workspace_index)?;
        Some(TabSurfaceTarget {
            workspace_index,
            tab_index: workspace.active_tab_index(),
        })
    });
    compute_tab_surface_for(
        app,
        terminal_runtimes,
        target,
        area,
        resize_panes,
        cell_size,
    )
}

pub(crate) fn compute_tab_surface_for(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    target: Option<TabSurfaceTarget>,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) -> TabSurfaceLayout {
    let tab = target.and_then(|target| {
        app.workspaces
            .get(target.workspace_index)?
            .tabs
            .get(target.tab_index)
    });
    let split_borders = tab
        .map(|tab| {
            if tab.zoomed {
                Vec::new()
            } else {
                tab.layout.splits(area)
            }
        })
        .unwrap_or_default();
    let pane_infos = target.map_or_else(Vec::new, |target| {
        compute_pane_infos_for_tab(
            app,
            terminal_runtimes,
            target.workspace_index,
            target.tab_index,
            area,
            resize_panes,
            cell_size,
        )
    });

    TabSurfaceLayout {
        target,
        pane_infos,
        split_borders,
    }
}

pub(crate) fn resize_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    workspace_index: usize,
    tab_index: usize,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let Some(tab) = app
        .workspaces
        .get(workspace_index)
        .and_then(|workspace| workspace.tabs.get(tab_index))
    else {
        return;
    };
    resize_tab_panes(
        app,
        terminal_runtimes,
        workspace_index,
        tab,
        area,
        cell_size,
    );
}

pub(crate) fn render_tab_surface(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
    frame: &mut Frame,
) {
    render_panes(
        app,
        terminal_runtimes,
        frame,
        surface.target,
        surface.pane_infos,
        surface.split_borders,
    );
}

pub(crate) fn tab_surface_hyperlinks(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Vec<((u16, u16), String, String)> {
    let Some(ws_idx) = surface.target.map(|target| target.workspace_index) else {
        return Vec::new();
    };
    if app.workspaces.get(ws_idx).is_none() {
        return Vec::new();
    }

    let mut links = Vec::new();
    for info in surface.pane_infos {
        if let Some(runtime) = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)
        {
            links.extend(runtime.visible_hyperlinks(info.inner_rect));
        }
    }
    links
}

pub(crate) fn tab_surface_cursor(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    surface: TabSurfaceView<'_>,
) -> Option<CursorState> {
    let ws_idx = surface.target?.workspace_index;
    let info = surface.pane_infos.iter().find(|info| info.is_focused)?;
    if !app.pane_exposes_host_cursor(ws_idx, info.id) {
        return None;
    }
    let runtime = app.runtime_for_pane_in_workspace(terminal_runtimes, ws_idx, info.id)?;
    if runtime.synchronized_output_active() {
        return None;
    }
    let scrolled_back = super::panes::pane_is_scrolled_back(runtime);
    let reveal = app.reveal_hidden_cursor_for_cjk_ime
        && (!app.cjk_ime_agent_filter_configured || {
            let detected = app
                .workspaces
                .get(ws_idx)
                .and_then(|ws| ws.terminal_id(info.id))
                .and_then(|terminal_id| app.terminals.get(terminal_id))
                .and_then(|terminal| terminal.detected_agent);
            detected.is_some_and(|agent| app.cjk_ime_agents.contains(&agent))
        });

    if let Some(cursor) = runtime.cursor_state(info.inner_rect, true) {
        let visible = if reveal {
            !scrolled_back
        } else {
            cursor.visible && !scrolled_back
        };
        Some(CursorState {
            x: cursor.x,
            y: cursor.y,
            visible,
            shape: if reveal && visible {
                app.cjk_ime_cursor_shape
            } else {
                cursor.shape
            },
        })
    } else if reveal && !scrolled_back {
        Some(CursorState {
            x: info.inner_rect.x,
            y: info.inner_rect.y,
            visible: true,
            shape: app.cjk_ime_cursor_shape,
        })
    } else {
        None
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::workspace::Workspace;
    use ratatui::backend::TestBackend;
    use ratatui::layout::Direction;
    use ratatui::Terminal;

    #[tokio::test]
    async fn explicit_surface_layout_drives_render_cursor_and_hyperlinks() {
        let uri = "https://example.com/surface";
        let mut workspace = Workspace::test_new("shell-workspace");
        let left = workspace.tabs[0].root_pane;
        let right = workspace.test_split(Direction::Horizontal);
        workspace.insert_test_runtime(
            left,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(
                20,
                8,
                format!("\x1b]8;;{uri}\x1b\\LEFT\x1b]8;;\x1b\\").as_bytes(),
            ),
        );
        workspace.insert_test_runtime(
            right,
            crate::terminal::TerminalRuntime::test_with_screen_bytes(20, 8, b"RIGHT"),
        );

        let mut app = AppState::test_new();
        app.workspaces = vec![workspace];
        app.active = Some(0);
        app.selected = 0;

        let full_area = Rect::new(0, 0, 106, 20);
        let area = full_area;
        let surface = compute_tab_surface(
            &app,
            &TerminalRuntimeRegistry::new(),
            area,
            false,
            crate::kitty_graphics::HostCellSize::default(),
        );
        assert_eq!(surface.pane_infos.len(), 2);
        assert!(!surface.split_borders.is_empty());

        app.view.terminal_area = Rect::new(9, 8, 7, 6);
        app.view.pane_infos.clear();

        let surface_view = TabSurfaceView {
            target: surface.target,
            pane_infos: &surface.pane_infos,
            split_borders: &surface.split_borders,
        };
        let mut terminal =
            Terminal::new(TestBackend::new(full_area.width, full_area.height)).unwrap();
        terminal
            .draw(|frame| {
                render_tab_surface(&app, &TerminalRuntimeRegistry::new(), surface_view, frame)
            })
            .unwrap();

        let rendered = terminal
            .backend()
            .buffer()
            .content()
            .iter()
            .map(|cell| cell.symbol())
            .collect::<String>();
        assert!(rendered.contains("LEFT"), "surface: {rendered:?}");
        assert!(rendered.contains("RIGHT"), "surface: {rendered:?}");
        assert!(!rendered.contains("shell-workspace"));

        let links = tab_surface_hyperlinks(&app, &TerminalRuntimeRegistry::new(), surface_view);
        assert!(links
            .iter()
            .any(|(_, symbol, link)| { symbol == "L" && link == uri }));
        assert!(tab_surface_cursor(&app, &TerminalRuntimeRegistry::new(), surface_view,).is_some());
    }
}
