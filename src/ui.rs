use ratatui::layout::Rect;

mod onboarding;
mod panes;
mod release_notes;
mod scrollbar;
mod sidebar;
mod status;
mod tab_surface;
mod text;
mod widgets;

pub(crate) use self::onboarding::{
    onboarding_welcome_continue_rect, ONBOARDING_DESCRIPTION, ONBOARDING_HELP_LABEL,
    ONBOARDING_HELP_SUFFIX, ONBOARDING_NEXT, ONBOARDING_PREFIX_LABEL, ONBOARDING_PREFIX_SUFFIX,
    ONBOARDING_SUBTITLE, ONBOARDING_TITLE,
};
#[cfg(all(test, unix))]
pub(crate) use self::panes::popup_pane_rects;
use self::panes::resize_popup_pane;
pub(crate) use self::panes::{
    apply_pane_chrome, pane_inner_rect, pane_is_scrolled_back, render_selection_highlight,
};
pub(crate) use self::release_notes::{
    product_announcement_display_lines, product_announcement_scroll_metrics,
    release_notes_close_button_rect, release_notes_display_lines, release_notes_scroll_metrics,
    PRODUCT_ANNOUNCEMENT_MODAL_SIZE, RELEASE_NOTES_MODAL_SIZE,
};
pub(crate) use self::scrollbar::{
    release_notes_scrollbar_rect, render_pane_scrollbar_buffer, render_scrollbar_buffer,
    scrollbar_offset_from_drag_row, scrollbar_offset_from_row, scrollbar_thumb,
    scrollbar_thumb_grab_offset,
};
pub(crate) use self::sidebar::{
    agent_panel_entries_from, expanded_sidebar_sections, resolved_token_spans, sidebar_agent_rows,
    sidebar_section_divider_rect, sidebar_space_rows, AgentPanelEntry, AgentTokenContext,
    ResolvedToken, ResolvedTokenKind, SpaceTokenContext,
};
use self::status::copy_feedback_rect;
pub(crate) use self::status::{render_config_diagnostic_buffer, render_copy_feedback_buffer};
pub(crate) use self::tab_surface::{
    compute_tab_surface, compute_tab_surface_for, render_tab_surface, resize_tab_surface,
    tab_surface_cursor, tab_surface_hyperlinks, TabSurfaceLayout, TabSurfaceTarget, TabSurfaceView,
};
pub(crate) use self::text::truncate_end;
pub(crate) use self::widgets::{centered_popup_rect, modal_stack_areas};

use crate::app::AppState;
use crate::terminal::TerminalRuntimeRegistry;

pub fn compute_view_with_runtime_registry(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        true,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

pub fn compute_view_with_cell_size(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    compute_view_internal(app, terminal_runtimes, area, true, cell_size);
}

pub(crate) fn compute_view_without_resizing_panes(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
) {
    compute_view_internal(
        app,
        terminal_runtimes,
        area,
        false,
        crate::kitty_graphics::HostCellSize::default(),
    );
}

fn compute_view_internal(
    app: &mut AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    resize_panes: bool,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    let TabSurfaceLayout { pane_infos, .. } =
        compute_tab_surface(app, terminal_runtimes, area, resize_panes, cell_size);

    if resize_panes {
        resize_background_tab_panes(app, terminal_runtimes, area, cell_size);
        resize_popup_pane(app, terminal_runtimes, area, cell_size);
    }

    app.view = crate::app::ViewState {
        terminal_area: area,
        pane_infos,
    };
}

fn resize_background_tab_panes(
    app: &AppState,
    terminal_runtimes: &TerminalRuntimeRegistry,
    area: Rect,
    cell_size: crate::kitty_graphics::HostCellSize,
) {
    for (workspace_index, workspace) in app.workspaces.iter().enumerate() {
        for tab_index in 0..workspace.tabs.len() {
            if app.active == Some(workspace_index) && tab_index == workspace.active_tab_index() {
                continue;
            }
            resize_tab_surface(
                app,
                terminal_runtimes,
                workspace_index,
                tab_index,
                area,
                cell_size,
            );
        }
    }
}

pub(crate) fn copy_feedback_offset_for_toast(
    area: Rect,
    feedback: &crate::app::state::CopyFeedback,
    base_offset: u16,
    position: crate::config::ToastClipboardPosition,
    toast_rect: Rect,
) -> u16 {
    let feedback_rect = copy_feedback_rect(area, feedback, base_offset, position);
    if rectangles_overlap(feedback_rect, toast_rect) {
        base_offset.saturating_add(toast_rect.height)
    } else {
        base_offset
    }
}

fn rectangles_overlap(left: Rect, right: Rect) -> bool {
    left.x < right.right()
        && right.x < left.right()
        && left.y < right.bottom()
        && right.y < left.bottom()
}
